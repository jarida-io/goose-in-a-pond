//! Which central holds the setup session, and the pump from its notifications to `tx`. Nothing here
//! names BlueZ, so the routing is tested against a fake radio.

use std::fmt::Debug;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::setup_channel::{SessionHandle, SessionWriter, SetupChannel};

/// BlueZ starts a new notification session for every subscriber, which stops the last one; a stop
/// that nothing replaces within this window means nobody is subscribed any more.
pub(super) const RESUBSCRIBE_GRACE: Duration = Duration::from_secs(1);
/// How long an answer waits for a subscriber on `tx` before the session gives the phone up.
pub(super) const SUBSCRIBE_WAIT: Duration = Duration::from_secs(5);

/// Why an `rx` write was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Another central holds the session.
    Busy,
    /// This central's session has ended; its link is being dropped.
    Ended,
}

/// The link layer as the router needs it.
#[async_trait]
pub(super) trait Radio: Send + Sync + 'static {
    type Central: Copy + Eq + Debug + Send + Sync + 'static;

    /// Resolves once this central's link is gone.
    async fn link_lost(&self, central: Self::Central);

    async fn disconnect(&self, central: Self::Central);
}

/// One notification session on `tx`.
#[async_trait]
pub(super) trait Notifier: Send + Sync + 'static {
    async fn notify(&self, chunk: &[u8]) -> Result<(), NotifierStopped>;

    /// Resolves once this notification session has stopped.
    async fn stopped(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct NotifierStopped;

#[derive(Clone)]
struct Subscription {
    generation: u64,
    notifier: Arc<dyn Notifier>,
}

struct Link<C> {
    central: C,
    generation: u64,
    writer: SessionWriter,
    att_mtu: u16,
}

struct State<C> {
    link: Option<Link<C>>,
    generations: u64,
}

pub(super) struct Router<R: Radio> {
    channel: Arc<SetupChannel>,
    radio: Arc<R>,
    state: Mutex<State<R::Central>>,
    subscription: watch::Sender<Option<Subscription>>,
}

impl<R: Radio> Router<R> {
    pub(super) fn new(channel: Arc<SetupChannel>, radio: Arc<R>) -> Arc<Self> {
        Arc::new(Self {
            channel,
            radio,
            state: Mutex::new(State {
                link: None,
                generations: 0,
            }),
            subscription: watch::Sender::new(None),
        })
    }

    /// One `rx` write. The first from a central opens its session; another central's write while
    /// that session is open is refused.
    pub(super) async fn on_write(
        self: &Arc<Self>,
        central: R::Central,
        att_mtu: u16,
        bytes: &[u8],
    ) -> Result<(), Refusal> {
        let (writer, mtu_changed) = self.writer_for(central, att_mtu)?;
        if mtu_changed {
            writer
                .set_att_mtu(att_mtu)
                .await
                .map_err(|_| Refusal::Ended)?;
        }
        writer.write(bytes).await.map_err(|_| Refusal::Ended)
    }

    fn writer_for(
        self: &Arc<Self>,
        central: R::Central,
        att_mtu: u16,
    ) -> Result<(SessionWriter, bool), Refusal> {
        let mut state = self.lock();
        if let Some(link) = state.link.as_mut() {
            if link.central == central {
                let changed = link.att_mtu != att_mtu;
                link.att_mtu = att_mtu;
                return Ok((link.writer.clone(), changed));
            }
            if self.channel.is_session_open() {
                tracing::debug!(?central, "refused a second central on the setup channel");
                return Err(Refusal::Busy);
            }
        }
        let SessionHandle {
            writer,
            notifications,
        } = self.channel.open().map_err(|_| Refusal::Busy)?;
        state.generations += 1;
        let generation = state.generations;
        state.link = Some(Link {
            central,
            generation,
            writer: writer.clone(),
            att_mtu,
        });
        drop(state);
        tracing::debug!(?central, "a central opened the setup channel over BLE");
        tokio::spawn(self.clone().pump(
            central,
            generation,
            SessionHandle {
                writer: writer.clone(),
                notifications,
            },
        ));
        Ok((writer, true))
    }

    /// A central subscribed to `tx`. BlueZ hands every subscriber the same session, so the newest
    /// notifier is the one to send through.
    pub(super) fn on_subscribe(self: &Arc<Self>, notifier: Arc<dyn Notifier>) {
        let generation = {
            let mut state = self.lock();
            state.generations += 1;
            state.generations
        };
        self.subscription.send_replace(Some(Subscription {
            generation,
            notifier: notifier.clone(),
        }));
        let router: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            notifier.stopped().await;
            tokio::time::sleep(RESUBSCRIBE_GRACE).await;
            if let Some(router) = router.upgrade() {
                router.unsubscribed(generation);
            }
        });
    }

    fn unsubscribed(&self, generation: u64) {
        let cleared = self.subscription.send_if_modified(|current| match current {
            Some(subscription) if subscription.generation == generation => {
                *current = None;
                true
            }
            _ => false,
        });
        if cleared {
            if let Some(link) = self.lock().link.as_ref() {
                tracing::debug!(central = ?link.central, "nobody listens on tx; ending the session");
                end_session(&link.writer);
            }
        }
    }

    /// Ends whatever session is open and forgets the subscription; the radio is going away.
    pub(super) fn shutdown(&self) {
        self.subscription.send_replace(None);
        if let Some(link) = self.lock().link.take() {
            end_session(&link.writer);
        }
    }

    /// Carries one session's notifications to `tx` until the session ends, then drops the phone's
    /// link and frees the slot for the next central.
    async fn pump(self: Arc<Self>, central: R::Central, generation: u64, handle: SessionHandle) {
        let SessionHandle {
            writer,
            mut notifications,
        } = handle;
        let mut subscription = self.subscription.subscribe();
        let radio = self.radio.clone();
        let link_lost = radio.link_lost(central);
        tokio::pin!(link_lost);
        let mut link_gone = false;
        let mut audible = true;
        loop {
            tokio::select! {
                chunk = notifications.next() => match chunk {
                    Some(chunk) => {
                        if audible && !deliver(&mut subscription, &chunk).await {
                            tracing::debug!(?central, "no subscriber on tx; ending the session");
                            audible = false;
                            end_session(&writer);
                        }
                    }
                    None => break,
                },
                () = &mut link_lost, if !link_gone => {
                    link_gone = true;
                    end_session(&writer);
                }
            }
        }
        let reason = notifications.end().map_or("unknown", |end| end.as_str());
        tracing::debug!(?central, reason, "the setup session over BLE ended");
        if !link_gone {
            self.radio.disconnect(central).await;
        }
        let mut state = self.lock();
        if state
            .link
            .as_ref()
            .is_some_and(|link| link.generation == generation)
        {
            state.link = None;
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<R::Central>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Sends one chunk through the current subscriber, waiting a little for one to (re)subscribe.
async fn deliver(subscription: &mut watch::Receiver<Option<Subscription>>, chunk: &[u8]) -> bool {
    let deadline = Instant::now() + SUBSCRIBE_WAIT;
    loop {
        let current = subscription.borrow_and_update().clone();
        if let Some(current) = current {
            if current.notifier.notify(chunk).await.is_ok() {
                return true;
            }
        }
        match tokio::time::timeout_at(deadline, subscription.changed()).await {
            Ok(Ok(())) => continue,
            _ => return false,
        }
    }
}

/// Spawned: the session's inbound queue can be full while its task waits on the pump, so waiting
/// here could wedge both.
fn end_session(writer: &SessionWriter) {
    let writer = writer.clone();
    tokio::spawn(async move { writer.phone_disconnected().await });
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{json, Value};
    use tokio::sync::Notify;

    use super::*;
    use crate::setup_channel::framing::{self, Reassembler};
    use crate::test_support::test_pond;

    const PHONE: u8 = 1;
    const STRANGER: u8 = 2;

    #[derive(Default)]
    struct FakeRadio {
        lost: Mutex<HashMap<u8, Arc<Notify>>>,
        disconnected: Mutex<Vec<u8>>,
    }

    impl FakeRadio {
        fn signal(&self, central: u8) -> Arc<Notify> {
            self.lost
                .lock()
                .unwrap()
                .entry(central)
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone()
        }

        fn drop_link(&self, central: u8) {
            self.signal(central).notify_one();
        }

        fn disconnected(&self) -> Vec<u8> {
            self.disconnected.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Radio for FakeRadio {
        type Central = u8;

        async fn link_lost(&self, central: u8) {
            self.signal(central).notified().await;
        }

        async fn disconnect(&self, central: u8) {
            self.disconnected.lock().unwrap().push(central);
        }
    }

    #[derive(Default)]
    struct FakeNotifier {
        sent: Mutex<Vec<Vec<u8>>>,
        heard: Notify,
        stop: watch::Sender<bool>,
    }

    impl FakeNotifier {
        fn stop(&self) {
            self.stop.send_replace(true);
        }

        fn take(&self) -> Vec<Vec<u8>> {
            std::mem::take(&mut *self.sent.lock().unwrap())
        }
    }

    #[async_trait]
    impl Notifier for FakeNotifier {
        async fn notify(&self, chunk: &[u8]) -> Result<(), NotifierStopped> {
            if *self.stop.borrow() {
                return Err(NotifierStopped);
            }
            self.sent.lock().unwrap().push(chunk.to_vec());
            self.heard.notify_one();
            Ok(())
        }

        async fn stopped(&self) {
            let mut stop = self.stop.subscribe();
            let _ = stop.wait_for(|stopped| *stopped).await;
        }
    }

    struct World {
        router: Arc<Router<FakeRadio>>,
        radio: Arc<FakeRadio>,
        channel: Arc<SetupChannel>,
    }

    async fn world() -> World {
        let channel = Arc::new(test_pond().channel);
        let radio = Arc::new(FakeRadio::default());
        World {
            router: Router::new(channel.clone(), radio.clone()),
            radio,
            channel,
        }
    }

    fn hello(id: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({"id": id, "type": "hello"})).unwrap()
    }

    async fn write(
        world: &World,
        central: u8,
        att_mtu: u16,
        message: &[u8],
    ) -> Result<(), Refusal> {
        for chunk in framing::split(message, att_mtu).unwrap() {
            world.router.on_write(central, att_mtu, &chunk).await?;
        }
        Ok(())
    }

    /// The next whole message on `tx`, checking every chunk fits the MTU.
    async fn answer(notifier: &FakeNotifier, att_mtu: u16) -> Value {
        let mut reassembler = Reassembler::new();
        loop {
            for chunk in notifier.take() {
                assert!(
                    chunk.len() <= framing::chunk_len(att_mtu),
                    "{}",
                    chunk.len()
                );
                if let Some(message) = reassembler.push(&chunk).unwrap() {
                    return serde_json::from_slice(&message).unwrap();
                }
            }
            tokio::time::timeout(Duration::from_secs(5), notifier.heard.notified())
                .await
                .expect("the pond answers");
        }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_phone_that_subscribes_and_writes_gets_its_answer_chunked_to_its_mtu() {
        for att_mtu in [23, 185] {
            let world = world().await;
            let notifier = Arc::new(FakeNotifier::default());
            world.router.on_subscribe(notifier.clone());

            write(&world, PHONE, att_mtu, &hello(1)).await.unwrap();
            let hello = answer(&notifier, att_mtu).await;
            assert_eq!(hello["ok"], true);
            assert_eq!(hello["pond_id"], "7f3a");
            assert!(world.channel.is_session_open());
        }
    }

    #[tokio::test]
    async fn a_second_central_is_refused_while_the_first_holds_the_session() {
        let world = world().await;
        let notifier = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(notifier.clone());
        write(&world, PHONE, 185, &hello(1)).await.unwrap();
        answer(&notifier, 185).await;

        assert_eq!(
            write(&world, STRANGER, 185, &hello(1)).await,
            Err(Refusal::Busy)
        );
        write(&world, PHONE, 185, &hello(2)).await.unwrap();
        assert_eq!(
            answer(&notifier, 185).await["id"],
            2,
            "the phone is unaffected"
        );
        assert!(world.radio.disconnected().is_empty(), "nobody was dropped");
    }

    #[tokio::test]
    async fn the_phone_leaving_frees_the_channel_for_the_next_central() {
        let world = world().await;
        let notifier = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(notifier.clone());
        write(&world, PHONE, 185, &hello(1)).await.unwrap();
        answer(&notifier, 185).await;

        world.radio.drop_link(PHONE);
        tokio::time::timeout(Duration::from_secs(5), async {
            while world.channel.is_session_open() {
                settle().await;
            }
        })
        .await
        .expect("the session ends when the link goes");
        settle().await;
        assert!(
            world.radio.disconnected().is_empty(),
            "a link that is already gone is not dropped again"
        );

        write(&world, STRANGER, 185, &hello(1)).await.unwrap();
        assert_eq!(answer(&notifier, 185).await["ok"], true);
    }

    #[tokio::test]
    async fn nobody_listening_on_tx_ends_the_session_and_drops_the_phone() {
        let world = world().await;
        tokio::time::pause();
        let notifier = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(notifier.clone());
        write(&world, PHONE, 185, &hello(1)).await.unwrap();
        answer(&notifier, 185).await;

        notifier.stop();
        tokio::time::sleep(RESUBSCRIBE_GRACE * 2).await;
        settle().await;
        assert!(!world.channel.is_session_open());
        assert_eq!(world.radio.disconnected(), vec![PHONE]);
    }

    #[tokio::test]
    async fn a_new_subscriber_replacing_the_notifier_keeps_the_session() {
        let world = world().await;
        tokio::time::pause();
        let first = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(first.clone());
        write(&world, PHONE, 185, &hello(1)).await.unwrap();
        answer(&first, 185).await;

        let second = Arc::new(FakeNotifier::default());
        first.stop();
        world.router.on_subscribe(second.clone());
        tokio::time::sleep(RESUBSCRIBE_GRACE * 2).await;
        assert!(world.channel.is_session_open());

        write(&world, PHONE, 185, &hello(2)).await.unwrap();
        assert_eq!(answer(&second, 185).await["id"], 2);
    }

    #[tokio::test]
    async fn an_answer_with_no_subscriber_waits_then_ends_the_session() {
        let world = world().await;
        tokio::time::pause();
        write(&world, PHONE, 23, &hello(1)).await.unwrap();
        tokio::time::sleep(SUBSCRIBE_WAIT * 2).await;
        settle().await;
        assert!(
            !world.channel.is_session_open(),
            "a central that never subscribes cannot hold the only session"
        );
        assert_eq!(world.radio.disconnected(), vec![PHONE]);
    }

    #[tokio::test]
    async fn a_late_subscriber_inside_the_wait_still_hears_the_answer() {
        let world = world().await;
        tokio::time::pause();
        write(&world, PHONE, 23, &hello(1)).await.unwrap();
        tokio::time::sleep(SUBSCRIBE_WAIT / 2).await;
        let notifier = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(notifier.clone());
        assert_eq!(answer(&notifier, 23).await["ok"], true);
    }

    #[tokio::test]
    async fn a_channel_the_household_switched_off_refuses_a_new_central() {
        let world = world().await;
        world.channel.set_accepting(false);
        assert_eq!(
            write(&world, PHONE, 23, &hello(1)).await,
            Err(Refusal::Busy)
        );
        world.channel.set_accepting(true);
        assert!(write(&world, PHONE, 23, &hello(1)).await.is_ok());
    }

    #[tokio::test]
    async fn shutdown_ends_the_open_session() {
        let world = world().await;
        let notifier = Arc::new(FakeNotifier::default());
        world.router.on_subscribe(notifier.clone());
        write(&world, PHONE, 185, &hello(1)).await.unwrap();
        answer(&notifier, 185).await;

        world.router.shutdown();
        tokio::time::timeout(Duration::from_secs(5), async {
            while world.channel.is_session_open() {
                settle().await;
            }
        })
        .await
        .expect("shutdown ends the session");
    }
}
