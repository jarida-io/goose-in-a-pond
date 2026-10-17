use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::sync::oneshot;

use super::*;
use crate::setup_channel::framing::{self, Reassembler};
use crate::test_support::{test_pond, TestPond};

const PIPE: usize = 1 << 20;

/// The helper's end of one launch: it reads the pond's commands and plays CoreBluetooth back.
struct FakeHelper {
    commands: Lines<BufReader<DuplexStream>>,
    events: Option<DuplexStream>,
    exit: Option<oneshot::Sender<HelperExit>>,
}

impl FakeHelper {
    async fn say(&mut self, event: Event) {
        let events = self.events.as_mut().expect("the helper is alive");
        events
            .write_all(&encode_line(&event).unwrap())
            .await
            .unwrap();
    }

    async fn come_up(&mut self) {
        self.say(Event::Ready { protocol: 1 }).await;
        self.say(Event::State {
            state: RadioState::PoweredOn,
        })
        .await;
        self.say(Event::ServiceAdded).await;
    }

    async fn next_command(&mut self) -> Command {
        let line = tokio::time::timeout(Duration::from_secs(5), self.commands.next_line())
            .await
            .expect("a command in time")
            .unwrap()
            .expect("the pond still talks to the helper");
        serde_json::from_str(&line).unwrap()
    }

    /// Writes one request on `link` chunk by chunk, then reassembles the answer it is notified.
    async fn ask(&mut self, link: u64, att_mtu: u16, request: Value) -> Value {
        let message = serde_json::to_vec(&request).unwrap();
        for chunk in framing::split(&message, att_mtu).unwrap() {
            self.say(Event::Write {
                link,
                data: Bytes(chunk),
            })
            .await;
        }
        let mut reassembler = Reassembler::new();
        loop {
            match self.next_command().await {
                Command::Notify { link: to, data } => {
                    assert_eq!(to, link);
                    assert!(data.0.len() <= framing::chunk_len(att_mtu));
                    if let Some(answer) = reassembler.push(&data.0).unwrap() {
                        return serde_json::from_slice(&answer).unwrap();
                    }
                }
                other => panic!("expected a notification, got {other:?}"),
            }
        }
    }

    fn die(&mut self, exit: HelperExit) {
        self.events = None;
        if let Some(sender) = self.exit.take() {
            let _ = sender.send(exit);
        }
    }
}

#[derive(Default)]
struct FakeLauncher {
    helpers: Mutex<Option<mpsc::UnboundedSender<FakeHelper>>>,
    launches: AtomicUsize,
}

impl HelperLauncher for FakeLauncher {
    fn launch(&self) -> io::Result<HelperProcess> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let (stdin, helper_stdin) = tokio::io::duplex(PIPE);
        let (helper_stdout, stdout) = tokio::io::duplex(PIPE);
        let (exit, exited) = oneshot::channel();
        let helper = FakeHelper {
            commands: BufReader::new(helper_stdin).lines(),
            events: Some(helper_stdout),
            exit: Some(exit),
        };
        if let Some(helpers) = self.helpers.lock().unwrap().as_ref() {
            let _ = helpers.send(helper);
        }
        Ok(HelperProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            exited: Box::pin(async move { exited.await.unwrap_or_default() }),
        })
    }
}

struct Rig {
    peripheral: HelperPeripheral,
    launcher: Arc<FakeLauncher>,
    helpers: mpsc::UnboundedReceiver<FakeHelper>,
    pond: TestPond,
}

impl Rig {
    async fn start(policy: RestartPolicy) -> Self {
        let launcher = Arc::new(FakeLauncher::default());
        let (helpers_tx, helpers) = mpsc::unbounded_channel();
        *launcher.helpers.lock().unwrap() = Some(helpers_tx);
        let peripheral = HelperPeripheral::new(launcher.clone(), policy);
        let pond = test_pond();
        peripheral
            .start(Arc::new(pond.channel.clone()))
            .await
            .unwrap();
        Self {
            peripheral,
            launcher,
            helpers,
            pond,
        }
    }

    async fn next_helper(&mut self) -> FakeHelper {
        tokio::time::timeout(Duration::from_secs(120), self.helpers.recv())
            .await
            .expect("a launch in time")
            .unwrap()
    }

    async fn wait_for(&self, wanted: impl Fn(&PeripheralStatus) -> bool) -> PeripheralStatus {
        for _ in 0..200 {
            let status = self.peripheral.status();
            if wanted(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("status stuck at {:?}", self.peripheral.status());
    }
}

fn subscribed(link: u64, att_mtu: u16) -> Event {
    Event::Subscribed {
        link,
        central: format!("central-{link}"),
        att_mtu,
    }
}

#[tokio::test]
async fn a_subscribed_phone_gets_a_session_and_hears_its_answers_as_notifications() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    assert_eq!(rig.peripheral.status(), PeripheralStatus::Off);
    helper.come_up().await;
    rig.wait_for(|s| *s == PeripheralStatus::Serving).await;

    for att_mtu in [23, 185] {
        let link = u64::from(att_mtu);
        helper.say(subscribed(link, att_mtu)).await;
        let hello = helper
            .ask(link, att_mtu, json!({"id": 7, "type": "hello"}))
            .await;
        assert_eq!(hello["ok"], true);
        assert_eq!(hello["pond_id"], "7f3a");
        helper.say(Event::Unsubscribed { link }).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!rig.pond.channel.is_session_open(), "unsubscribing ends it");
    }
}

#[tokio::test]
async fn a_second_phone_is_refused_while_one_is_connected() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.come_up().await;
    helper.say(subscribed(1, 185)).await;
    helper.ask(1, 185, json!({"id": 1, "type": "hello"})).await;

    helper.say(subscribed(2, 185)).await;
    assert_eq!(helper.next_command().await, Command::Disconnect { link: 2 });

    // Leaving needs no refusal: the next command is the new phone's answer, not a disconnect.
    helper.say(Event::Unsubscribed { link: 1 }).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    helper.say(subscribed(3, 185)).await;
    let hello = helper.ask(3, 185, json!({"id": 2, "type": "hello"})).await;
    assert_eq!(hello["ok"], true);
}

#[tokio::test]
async fn a_session_that_ends_releases_its_link() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.come_up().await;
    helper.say(subscribed(5, 23)).await;
    helper
        .say(Event::Write {
            link: 5,
            data: Bytes(vec![0xff, 0xff, 0xff, 0xff]),
        })
        .await;
    assert_eq!(helper.next_command().await, Command::Disconnect { link: 5 });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!rig.pond.channel.is_session_open());
}

#[tokio::test]
async fn advertising_follows_the_pond_and_the_switch_refuses_phones() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.come_up().await;
    rig.wait_for(|s| *s == PeripheralStatus::Serving).await;
    rig.peripheral
        .advertise(Some(Advertisement {
            local_name: "Pond 7f3a".into(),
        }))
        .await
        .unwrap();
    assert_eq!(
        helper.next_command().await,
        Command::Advertise {
            local_name: "Pond 7f3a".into()
        }
    );
    helper
        .say(Event::Advertising {
            local_name: "Pond 7f3a".into(),
        })
        .await;
    rig.wait_for(|s| *s == PeripheralStatus::Advertising).await;

    rig.peripheral.advertise(None).await.unwrap();
    assert_eq!(helper.next_command().await, Command::StopAdvertising);
    helper.say(Event::AdvertisingStopped).await;
    rig.wait_for(|s| *s == PeripheralStatus::Serving).await;

    rig.pond.channel.set_accepting(false);
    helper.say(subscribed(9, 185)).await;
    assert_eq!(helper.next_command().await, Command::Disconnect { link: 9 });
}

#[tokio::test]
async fn the_radio_state_reaches_the_status_with_its_reason() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.say(Event::Ready { protocol: 1 }).await;
    helper
        .say(Event::State {
            state: RadioState::Unauthorized,
        })
        .await;
    let status = rig
        .wait_for(|s| matches!(s, PeripheralStatus::Unavailable { .. }))
        .await;
    assert!(status
        .reason()
        .unwrap()
        .contains("Privacy & Security > Bluetooth"));

    helper
        .say(Event::State {
            state: RadioState::PoweredOn,
        })
        .await;
    helper
        .say(Event::Error {
            context: "add_service".into(),
            message: "The specified UUID is not allowed".into(),
        })
        .await;
    let status = rig
        .wait_for(|s| s.reason().is_some_and(|r| r.contains("UUID")))
        .await;
    assert!(status
        .reason()
        .unwrap()
        .starts_with("macOS refused the setup service"));
}

#[tokio::test]
async fn a_radio_that_never_answers_says_whether_permission_is_pending() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.say(Event::Ready { protocol: 1 }).await;
    helper
        .say(Event::Authorization {
            authorization: protocol::Authorization::NotDetermined,
        })
        .await;
    assert_eq!(rig.peripheral.status(), PeripheralStatus::Off);
    helper
        .say(Event::Error {
            context: "radio".into(),
            message: "CoreBluetooth has not reported the radio's state after 10 s".into(),
        })
        .await;
    let status = rig
        .wait_for(|s| matches!(s, PeripheralStatus::Unavailable { .. }))
        .await;
    assert!(status
        .reason()
        .unwrap()
        .contains("waiting for someone to allow Bluetooth"));

    helper.come_up().await;
    rig.wait_for(|s| *s == PeripheralStatus::Serving).await;
}

#[tokio::test(start_paused = true)]
async fn a_helper_that_dies_ends_the_session_and_comes_back_advertising() {
    let mut rig = Rig::start(RestartPolicy::default()).await;
    let mut helper = rig.next_helper().await;
    helper.come_up().await;
    rig.peripheral
        .advertise(Some(Advertisement {
            local_name: "Pond 7f3a".into(),
        }))
        .await
        .unwrap();
    helper.next_command().await;
    helper.say(subscribed(1, 185)).await;
    helper.ask(1, 185, json!({"id": 1, "type": "hello"})).await;
    assert!(rig.pond.channel.is_session_open());

    helper.die(HelperExit {
        code: Some(1),
        signal: None,
        last_words: Some("CoreBluetooth went away".into()),
    });
    let status = rig
        .wait_for(|s| matches!(s, PeripheralStatus::Unavailable { .. }))
        .await;
    assert_eq!(
        status.reason(),
        Some("the Bluetooth helper exited with status 1: CoreBluetooth went away")
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !rig.pond.channel.is_session_open(),
        "the phone's session ended with it"
    );
    rig.peripheral
        .advertise(Some(Advertisement {
            local_name: "Pond 7f3a".into(),
        }))
        .await
        .expect("a restarting helper takes the wish with it");

    let mut again = rig.next_helper().await;
    assert_eq!(
        again.next_command().await,
        Command::Advertise {
            local_name: "Pond 7f3a".into()
        },
        "the restarted helper resumes what the pond asked for"
    );
    assert_eq!(rig.launcher.launches.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_helper_macos_keeps_killing_is_given_up_on_after_a_few_tries() {
    let policy = RestartPolicy::default();
    let mut rig = Rig::start(policy).await;
    let started = Instant::now();
    for _ in 0..=policy.max_restarts {
        let mut helper = rig.next_helper().await;
        helper.say(Event::Ready { protocol: 1 }).await;
        helper.die(HelperExit {
            code: None,
            signal: Some(SIGKILL),
            last_words: None,
        });
    }
    let status = rig
        .wait_for(|s| matches!(s, PeripheralStatus::Unavailable { .. }))
        .await;
    assert!(status.reason().unwrap().contains("no Bluetooth permission"));
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let refused = rig.peripheral.advertise(None).await.unwrap_err();
    assert!(
        refused.to_string().contains("no Bluetooth permission"),
        "{refused}"
    );
    assert_eq!(
        rig.launcher.launches.load(Ordering::SeqCst),
        1 + policy.max_restarts as usize,
        "no respawn loop"
    );
    let waited: Duration = (0..policy.max_restarts).map(|n| policy.backoff(n)).sum();
    assert!(started.elapsed() >= waited);
    assert_eq!(
        (0..4)
            .map(|n| policy.backoff(n).as_secs())
            .collect::<Vec<_>>(),
        [2, 8, 30, 30]
    );
}

#[tokio::test(start_paused = true)]
async fn a_helper_that_ran_for_a_while_gets_its_budget_back() {
    let policy = RestartPolicy {
        max_restarts: 1,
        ..RestartPolicy::default()
    };
    let mut rig = Rig::start(policy).await;
    for _ in 0..4 {
        let mut helper = rig.next_helper().await;
        helper.come_up().await;
        tokio::time::sleep(policy.healthy_uptime).await;
        helper.die(HelperExit {
            code: Some(70),
            ..HelperExit::default()
        });
    }
    assert_eq!(rig.launcher.launches.load(Ordering::SeqCst), 4);
}

#[test]
fn a_kill_before_the_radio_answered_is_read_as_a_missing_grant() {
    let killed = HelperExit {
        code: None,
        signal: Some(SIGKILL),
        last_words: None,
    };
    assert!(describe_exit(&killed, false).contains("no Bluetooth permission"));
    let aborted = HelperExit {
        signal: Some(SIGABRT),
        ..killed.clone()
    };
    assert!(describe_exit(&aborted, false).contains("(signal 6)"));
    assert_eq!(
        describe_exit(&killed, true),
        "the Bluetooth helper was stopped by signal 9"
    );
    assert_eq!(
        describe_exit(&HelperExit::default(), false),
        "the Bluetooth helper stopped"
    );
}
