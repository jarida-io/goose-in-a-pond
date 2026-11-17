//! Where a route or a member's turn hands what happened to whoever checks the heartbeat's
//! intentions. In-process only: a turn taken in another process is not heard here.

use std::sync::LazyLock;

use tokio::sync::broadcast;

use super::intentions::ConditionEvent;

/// Events held for a listener that has fallen behind; older ones are dropped.
const HELD: usize = 256;

static EVENTS: LazyLock<broadcast::Sender<ConditionEvent>> =
    LazyLock::new(|| broadcast::channel(HELD).0);

/// Hands `event` to every listener; with none listening it is dropped.
pub fn raise(event: ConditionEvent) {
    let _ = EVENTS.send(event);
}

/// The events raised from now on.
pub fn listen() -> broadcast::Receiver<ConditionEvent> {
    EVENTS.subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::todo::Transition;

    #[tokio::test]
    async fn a_raised_event_reaches_every_listener_and_nobody_listening_is_no_error() {
        let crossing = ConditionEvent::Place {
            place_id: "a-place-only-this-test-names".into(),
            transition: Transition::Arrive,
        };
        raise(crossing.clone());
        let (mut first, mut second) = (listen(), listen());
        raise(crossing.clone());
        for listener in [&mut first, &mut second] {
            let heard = loop {
                match listener.recv().await {
                    Ok(event) if event == crossing => break event,
                    Err(broadcast::error::RecvError::Closed) => panic!("the channel closed"),
                    _ => continue,
                }
            };
            assert_eq!(heard, crossing);
        }
    }
}
