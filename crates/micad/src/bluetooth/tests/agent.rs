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

/// A stand-in for bluetoothd's agent manager: it records who registered.
struct FakeAgentManager {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.bluez.AgentManager1")]
impl FakeAgentManager {
    fn register_agent(&self, agent: zbus::zvariant::ObjectPath<'_>, capability: String) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("register {agent} {capability}"));
    }

    fn request_default_agent(&self, agent: zbus::zvariant::ObjectPath<'_>) {
        self.calls.lock().unwrap().push(format!("default {agent}"));
    }
}

/// Bluetooth is off until the settings turn it on, so bluetoothd appears long
/// after micad started, and it forgets its agents when it restarts. The agent
/// is registered each time `org.bluez` gains an owner, not once at start.
#[tokio::test(flavor = "multi_thread")]
async fn the_agent_registers_each_time_bluetoothd_appears() {
    use std::io::BufRead;
    // A private bus: this asserts real name ownership, and must not skip.
    let mut bus = std::process::Command::new("dbus-daemon")
        .args(["--session", "--print-address=1", "--nofork"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon is needed for this test");
    let mut address = String::new();
    std::io::BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let connect = || async {
        zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap()
    };

    // micad's side, with no bluetoothd on the bus yet.
    let watcher = tokio::spawn(keep_agent_registered(connect().await));

    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let wait_for = |count: usize| {
        let calls = std::sync::Arc::clone(&calls);
        async move {
            tokio::time::timeout(Duration::from_secs(10), async {
                while calls.lock().unwrap().len() < count {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{count} calls, got {:?}", calls.lock().unwrap()));
        }
    };
    for round in 1..=2 {
        // bluetoothd starts, or starts again.
        let bluez = connect().await;
        bluez
            .object_server()
            .at(
                "/org/bluez",
                FakeAgentManager {
                    calls: std::sync::Arc::clone(&calls),
                },
            )
            .await
            .unwrap();
        bluez.request_name("org.bluez").await.unwrap();
        wait_for(round * 2).await;
        bluez.release_name("org.bluez").await.unwrap();
    }

    let expected = [
        format!("register {AGENT_PATH} {AGENT_CAPABILITY}"),
        format!("default {AGENT_PATH}"),
    ];
    assert_eq!(
        *calls.lock().unwrap(),
        [expected.clone(), expected].concat()
    );
    watcher.abort();
    let _ = bus.kill();
    let _ = bus.wait();
}
