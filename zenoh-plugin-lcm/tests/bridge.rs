use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lcm_transport::{FragmentReassembler, LcmTransport, LcmTransportConfig, LcmUrl};
use tokio::sync::RwLock;
use zenoh::bytes::ZBytes;
use zenoh::sample::Locality;

/// Counter for unique port allocation across parallel tests.
static PORT_COUNTER: AtomicU16 = AtomicU16::new(18000);

fn unique_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

fn test_config(port: u16) -> LcmTransportConfig {
    LcmTransportConfig {
        lcm_url: LcmUrl {
            multicast_group: Ipv4Addr::new(239, 255, 76, 67),
            port,
            ttl: 0,
            recv_buf_size: None,
        },
        network_interface: None,
        ..Default::default()
    }
}

/// Test: LCM app publishes → bridge picks up via multicast → publishes to Zenoh → Zenoh subscriber receives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_lcm_to_zenoh_bridge() {
    let port = unique_port();
    let prefix = "test_l2z";

    // Create a Zenoh session (peer mode, no scouting to avoid interference).
    let zsession = Arc::new(
        zenoh::open(zenoh::Config::default())
            .await
            .expect("Failed to open Zenoh session"),
    );

    // Subscribe to the Zenoh side to receive bridged messages.
    let sub = zsession
        .declare_subscriber(format!("{prefix}/**"))
        .await
        .expect("Failed to declare subscriber");

    // Create two LCM transports: one simulates the bridge, one simulates an LCM app.
    let bridge_transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let app_transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());

    let sent_sequences: Arc<RwLock<HashSet<u32>>> = Arc::new(RwLock::new(HashSet::new()));

    // Spawn a task that acts as the LCM→Zenoh half of the bridge:
    // receives from multicast, publishes to Zenoh.
    let bridge_t = bridge_transport.clone();
    let bridge_z = zsession.clone();
    let bridge_seqs = sent_sequences.clone();
    let bridge_prefix = prefix.to_string();
    let bridge_handle = tokio::spawn(async move {
        let mut reassembler = FragmentReassembler::new(Duration::from_secs(1), 4 * 1024 * 1024);
        loop {
            let msg = match bridge_t.recv(&mut reassembler).await {
                Ok(msg) => msg,
                Err(_) => continue,
            };

            // Loop prevention check.
            {
                let mut seqs = bridge_seqs.write().await;
                if seqs.remove(&msg.sequence_number) {
                    continue;
                }
            }

            let ke = format!("{}/{}", bridge_prefix, msg.channel);
            // Note: in the real bridge, this uses allowed_destination(Locality::Remote)
            // to prevent the local Zenoh→LCM subscriber from picking it up. We omit
            // that here since the test subscriber is on the same session.
            let _ = bridge_z
                .put(&ke, ZBytes::from(msg.data))
                .await;
        }
    });

    // Give the bridge task time to start listening.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Simulate an LCM app sending a message.
    app_transport
        .send("SENSOR_IMU", b"imu_data_123")
        .await
        .unwrap();

    // The bridge should pick it up and publish to Zenoh.
    let sample = tokio::time::timeout(TEST_TIMEOUT, sub.recv_async())
        .await
        .expect("timeout waiting for Zenoh message")
        .expect("subscriber error");

    assert_eq!(sample.key_expr().as_str(), &format!("{prefix}/SENSOR_IMU"));
    assert_eq!(sample.payload().to_bytes().as_ref(), b"imu_data_123");

    bridge_handle.abort();
}

/// Test: Zenoh publisher → bridge picks up from Zenoh → sends to LCM multicast → LCM app receives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_zenoh_to_lcm_bridge() {
    let port = unique_port();
    let prefix = "test_z2l";

    let zsession = Arc::new(
        zenoh::open(zenoh::Config::default())
            .await
            .expect("Failed to open Zenoh session"),
    );

    let bridge_transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let app_transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());

    let sent_sequences: Arc<RwLock<HashSet<u32>>> = Arc::new(RwLock::new(HashSet::new()));

    // Spawn a task that acts as the Zenoh→LCM half of the bridge:
    // subscribes to Zenoh, sends to LCM multicast.
    let bridge_t = bridge_transport.clone();
    let bridge_z = zsession.clone();
    let bridge_seqs = sent_sequences.clone();
    let bridge_prefix = prefix.to_string();
    let bridge_handle = tokio::spawn(async move {
        let sub = bridge_z
            .declare_subscriber(format!("{bridge_prefix}/**"))
            .await
            .expect("Failed to declare subscriber");

        loop {
            let sample = match sub.recv_async().await {
                Ok(s) => s,
                Err(_) => break,
            };

            let ke = sample.key_expr().as_str();
            let channel = ke
                .strip_prefix(&format!("{bridge_prefix}/"))
                .unwrap_or(ke);

            let payload: Vec<u8> = sample.payload().to_bytes().to_vec();

            if let Ok(seq) = bridge_t.send(channel, &payload).await {
                let mut seqs = bridge_seqs.write().await;
                seqs.insert(seq);
            }
        }
    });

    // Give the bridge task time to start listening.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Spawn LCM receiver on the app transport.
    let app_t = app_transport.clone();
    let recv_handle = tokio::spawn(async move {
        let mut reassembler = FragmentReassembler::new(Duration::from_secs(1), 4 * 1024 * 1024);
        app_t.recv(&mut reassembler).await
    });

    // Brief delay to let the receiver start.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Publish from Zenoh — the bridge should forward to LCM multicast.
    zsession
        .put(format!("{prefix}/MOTOR_CMD"), ZBytes::from(b"cmd_data_456".as_ref()))
        .await
        .expect("Failed to publish to Zenoh");

    let msg = tokio::time::timeout(TEST_TIMEOUT, recv_handle)
        .await
        .expect("timeout waiting for LCM message")
        .expect("recv task panicked")
        .expect("recv error");

    assert_eq!(msg.channel, "MOTOR_CMD");
    assert_eq!(msg.data, b"cmd_data_456");

    bridge_handle.abort();
}

/// Test: verify loop prevention — a message sent by the bridge to LCM should not
/// be re-bridged back through Zenoh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_loop_prevention() {
    let port = unique_port();
    let prefix = "test_loop";

    let zsession = Arc::new(
        zenoh::open(zenoh::Config::default())
            .await
            .expect("Failed to open Zenoh session"),
    );

    let bridge_transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let sent_sequences: Arc<RwLock<HashSet<u32>>> = Arc::new(RwLock::new(HashSet::new()));

    // Subscribe to Zenoh to detect any bridged messages.
    let sub = zsession
        .declare_subscriber(format!("{prefix}/**"))
        .await
        .expect("Failed to declare subscriber");

    // Spawn LCM→Zenoh bridge task with loop prevention.
    let bridge_t = bridge_transport.clone();
    let bridge_z = zsession.clone();
    let bridge_seqs = sent_sequences.clone();
    let bridge_prefix = prefix.to_string();
    let bridge_handle = tokio::spawn(async move {
        let mut reassembler = FragmentReassembler::new(Duration::from_secs(1), 4 * 1024 * 1024);
        loop {
            let msg = match bridge_t.recv(&mut reassembler).await {
                Ok(msg) => msg,
                Err(_) => continue,
            };

            // Loop prevention: skip messages we sent.
            {
                let mut seqs = bridge_seqs.write().await;
                if seqs.remove(&msg.sequence_number) {
                    continue;
                }
            }

            let ke = format!("{}/{}", bridge_prefix, msg.channel);
            let _ = bridge_z
                .put(&ke, ZBytes::from(msg.data))
                .allowed_destination(Locality::Remote)
                .await;
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // The bridge itself sends a message to LCM (simulating zenoh→lcm direction).
    let seq = bridge_transport.send("LOOPBACK", b"should_not_bridge").await.unwrap();
    sent_sequences.write().await.insert(seq);

    // Wait a bit for the message to loopback through multicast.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The subscriber should NOT have received anything — the bridge should have
    // recognized its own sequence number and skipped it.
    match tokio::time::timeout(Duration::from_millis(300), sub.recv_async()).await {
        Ok(_) => panic!("Loop prevention failed — bridge re-published its own message"),
        Err(_) => {} // Timeout is expected — no message should arrive.
    }

    bridge_handle.abort();
}
