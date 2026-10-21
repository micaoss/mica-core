use super::*;
use std::sync::Arc;

/// An agent with its pairing window open.
async fn open_agent() -> Arc<Agent> {
    let agent = Arc::new(Agent::default());
    agent.open_window().await;
    agent
}

/// Spawn a request from `address` and wait until the console can see it.
async fn request(agent: &Arc<Agent>, address: &str, passkey: u32) -> tokio::task::JoinHandle<bool> {
    let waiting = Arc::clone(agent);
    let address = address.to_string();
    let seen = address.clone();
    let task = tokio::spawn(async move { waiting.confirm(address, Some(passkey)).await });
    while !agent
        .pending()
        .await
        .iter()
        .any(|pending| pending.address == seen)
    {
        tokio::task::yield_now().await;
    }
    task
}

/// The console's answer is what the agent returns to BlueZ.
#[tokio::test]
async fn a_confirmed_request_is_accepted_and_a_refused_one_is_not() {
    for accept in [true, false] {
        let agent = open_agent().await;
        let pairing = request(&agent, "AA:BB:CC:DD:EE:01", 123_456).await;

        assert_eq!(agent.pending().await[0].passkey, Some(123_456));
        assert!(agent.answer("AA:BB:CC:DD:EE:01", accept).await);

        assert_eq!(pairing.await.expect("the pairing task"), accept);
        assert!(
            agent.pending().await.is_empty(),
            "the request was not cleared"
        );
    }
}

/// Requests wait side by side, each answered by its own address.
#[tokio::test]
async fn each_waiting_device_is_answered_by_its_address() {
    let agent = open_agent().await;
    let first = request(&agent, "AA:BB:CC:DD:EE:01", 1).await;
    let second = request(&agent, "AA:BB:CC:DD:EE:02", 2).await;
    assert_eq!(agent.pending().await.len(), 2);

    assert!(!agent.answer("AA:BB:CC:DD:EE:03", true).await);
    assert!(agent.answer("AA:BB:CC:DD:EE:02", false).await);
    assert!(agent.answer("AA:BB:CC:DD:EE:01", true).await);
    assert!(first.await.expect("the first"));
    assert!(!second.await.expect("the second"));
}

/// Outside a window nobody is looking: a request is refused at once.
#[tokio::test]
async fn a_request_outside_the_window_is_refused() {
    let agent = Agent::default();
    assert!(
        !agent
            .confirm("AA:BB:CC:DD:EE:01".to_string(), Some(1))
            .await
    );
    assert!(agent.pending().await.is_empty());
}

/// Closing the window refuses what is still waiting; the timer of an earlier
/// opening does not close a later one.
#[tokio::test]
async fn closing_the_window_refuses_what_waits() {
    let agent = Arc::new(Agent::default());
    let first = agent.open_window().await;
    let second = agent.open_window().await;
    assert!(
        !agent.expire(first).await,
        "an earlier timer closed a later window"
    );
    assert_eq!(agent.window().await, Some(second));
    assert!(second.expires_at >= first.expires_at);

    let pairing = request(&agent, "AA:BB:CC:DD:EE:01", 1).await;
    assert!(agent.expire(second).await);
    assert!(!pairing.await.expect("the pairing task"));
    assert!(agent.pending().await.is_empty());
    assert_eq!(agent.window().await, None);
}

/// The address is read out of the object path BlueZ names a device by.
#[test]
fn an_address_is_read_out_of_the_object_path() {
    let path =
        zbus::ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").expect("a device path");

    assert_eq!(address_of(&path), "AA:BB:CC:DD:EE:FF");
}

/// A stand-in for bluetoothd's agent manager: it records who registered.
struct FakeAgentManager {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.bluez.AgentManager1")]
impl FakeAgentManager {
    fn register_agent(&self, agent: zbus::ObjectPath<'_>, capability: String) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("register {agent} {capability}"));
    }

    fn request_default_agent(&self, agent: zbus::ObjectPath<'_>) {
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
            .build()
            .await
            .unwrap()
    };

    // micad's side, with no bluetoothd on the bus yet.
    let watcher = tokio::spawn(keep_agent_registered(
        connect().await,
        Arc::new(Agent::default()),
    ));

    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let wait_for = |count: usize| {
        let calls = std::sync::Arc::clone(&calls);
        async move {
            tokio::time::timeout(BUS_CALL, async {
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

/// How long one call to the private bus may take. It answers in milliseconds,
/// but shares the machine with the rest of the gate.
const BUS_CALL: std::time::Duration = std::time::Duration::from_secs(30);

/// What BlueZ calls is the recorded `org.bluez.Agent1`, as zbus answers an
/// introspection of the served agent.
#[test]
fn the_agent_serves_the_recorded_interface() {
    use zbus::object_server::Interface;
    let agent = PairingAgent {
        state: Arc::new(Agent::default()),
    };
    let mut xml = String::new();
    agent.introspect_to_writer(&mut xml, 0);

    let open = "<interface name=\"org.bluez.Agent1\">";
    let start = xml
        .find(open)
        .unwrap_or_else(|| panic!("no Agent1 in:\n{xml}"));
    let end = start + xml[start..].find("</interface>").unwrap() + "</interface>".len();
    let served: String = xml[start..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("{line}\n"))
        .collect();
    let recorded = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/introspection/org.bluez.Agent1.xml"),
    )
    .unwrap();
    assert_eq!(
        served, recorded,
        "org.bluez.Agent1 differs from its recorded definition"
    );
}
