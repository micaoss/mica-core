use super::*;
use std::sync::Arc;

/// The console's answer is what the agent returns to BlueZ.
#[tokio::test]
async fn a_confirmed_request_is_accepted_and_a_refused_one_is_not() {
    for accept in [true, false] {
        let agent = Arc::new(Agent::with_pin("4211".to_string()));
        let waiting = Arc::clone(&agent);
        let pairing = tokio::spawn(async move {
            waiting
                .confirm("AA:BB:CC:DD:EE:01".to_string(), 123_456)
                .await
        });

        // The request is visible while it waits: that is what the console
        // renders the dialog from.
        let pending = loop {
            if let Some(pending) = agent.pending().await {
                break pending;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(pending.passkey, 123_456);
        assert!(agent.answer("AA:BB:CC:DD:EE:01", accept).await);

        assert_eq!(pairing.await.expect("the pairing task"), accept);
        assert!(agent.pending().await.is_none(), "the slot was not cleared");
    }
}

/// An answer for a device that is not the one waiting changes nothing: two
/// dialogs cannot be told apart, so the address has to match.
#[tokio::test]
async fn an_answer_for_another_device_is_ignored() {
    let agent = Arc::new(Agent::with_pin("4211".to_string()));
    let waiting = Arc::clone(&agent);
    let pairing =
        tokio::spawn(async move { waiting.confirm("AA:BB:CC:DD:EE:01".to_string(), 1).await });
    while agent.pending().await.is_none() {
        tokio::task::yield_now().await;
    }

    assert!(!agent.answer("AA:BB:CC:DD:EE:02", true).await);
    assert!(agent.pending().await.is_some(), "the request was consumed");

    assert!(agent.answer("AA:BB:CC:DD:EE:01", true).await);
    assert!(pairing.await.expect("the pairing task"));
}

/// A second request while one is pending is refused rather than queued.
#[tokio::test]
async fn a_second_request_is_refused_while_one_is_pending() {
    let agent = Arc::new(Agent::with_pin("4211".to_string()));
    let waiting = Arc::clone(&agent);
    let first =
        tokio::spawn(async move { waiting.confirm("AA:BB:CC:DD:EE:01".to_string(), 1).await });
    while agent.pending().await.is_none() {
        tokio::task::yield_now().await;
    }

    assert!(!agent.confirm("AA:BB:CC:DD:EE:02".to_string(), 2).await);

    agent.answer("AA:BB:CC:DD:EE:01", true).await;
    assert!(first.await.expect("the pairing task"));
}

/// The address is read out of the object path BlueZ names a device by.
#[test]
fn an_address_is_read_out_of_the_object_path() {
    let path = zbus::zvariant::ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF")
        .expect("a device path");

    assert_eq!(address_of(&path), "AA:BB:CC:DD:EE:FF");
}

/// The PIN the agent answers with is the one it was last given.
#[tokio::test]
async fn the_pin_is_whatever_the_settings_last_said() {
    let agent = Agent::with_pin("0000".to_string());
    assert_eq!(agent.pin().await, "0000");

    agent.set_pin("4211".to_string()).await;

    assert_eq!(agent.pin().await, "4211");
}
