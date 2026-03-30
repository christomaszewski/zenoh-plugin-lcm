use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lcm_transport::{FragmentReassembler, LcmTransport, LcmTransportConfig, LcmUrl};

/// Counter for unique port allocation across parallel tests.
static PORT_COUNTER: AtomicU16 = AtomicU16::new(17000);

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

fn test_config_with_mtu(port: u16, mtu: usize) -> LcmTransportConfig {
    LcmTransportConfig {
        fragment_mtu: mtu,
        ..test_config(port)
    }
}

fn new_reassembler() -> FragmentReassembler {
    FragmentReassembler::new(Duration::from_secs(2), 4 * 1024 * 1024)
}

/// Helper: send a message, then receive it back via multicast loopback.
async fn send_and_recv(
    transport: &Arc<LcmTransport>,
    channel: &str,
    data: &[u8],
) -> lcm_transport::LcmMessage {
    let t = transport.clone();
    let chan = channel.to_string();
    let payload = data.to_vec();

    let recv_handle = tokio::spawn({
        let t = t.clone();
        async move {
            let mut r = new_reassembler();
            t.recv(&mut r).await
        }
    });

    // Brief yield to let the recv task start listening.
    tokio::time::sleep(Duration::from_millis(20)).await;

    t.send(&chan, &payload).await.expect("send failed");

    tokio::time::timeout(TEST_TIMEOUT, recv_handle)
        .await
        .expect("timeout waiting for message")
        .expect("recv task panicked")
        .expect("recv returned error")
}

// ---------- Tests ----------

#[tokio::test]
async fn test_short_message_loopback() {
    let port = unique_port();
    let transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let msg = send_and_recv(&transport, "TEST_CHAN", b"hello").await;

    assert_eq!(msg.channel, "TEST_CHAN");
    assert_eq!(msg.data, b"hello");
    assert_eq!(msg.sequence_number, 0);
}

#[tokio::test]
async fn test_empty_payload() {
    let port = unique_port();
    let transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let msg = send_and_recv(&transport, "EMPTY", b"").await;

    assert_eq!(msg.channel, "EMPTY");
    assert!(msg.data.is_empty());
}

#[tokio::test]
async fn test_fragmented_message_loopback() {
    let port = unique_port();
    // Default MTU is 1400, so a 5000-byte payload will fragment.
    let transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let payload: Vec<u8> = (0..5000).map(|i| (i % 256) as u8).collect();

    let msg = send_and_recv(&transport, "FRAG_TEST", &payload).await;

    assert_eq!(msg.channel, "FRAG_TEST");
    assert_eq!(msg.data, payload);
}

#[tokio::test]
async fn test_many_fragments() {
    let port = unique_port();
    // Small MTU forces many fragments.
    let transport = Arc::new(
        LcmTransport::new(test_config_with_mtu(port, 200))
            .await
            .unwrap(),
    );
    let payload: Vec<u8> = vec![0xAB; 10_000];

    let msg = send_and_recv(&transport, "MANY_FRAGS", &payload).await;

    assert_eq!(msg.channel, "MANY_FRAGS");
    assert_eq!(msg.data.len(), 10_000);
    assert_eq!(msg.data, payload);
}

#[tokio::test]
async fn test_multiple_channels() {
    let port = unique_port();
    let transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());

    let t = transport.clone();
    let recv_handle = tokio::spawn(async move {
        let mut reassembler = new_reassembler();
        let mut msgs = Vec::new();
        for _ in 0..3 {
            let msg = t.recv(&mut reassembler).await.unwrap();
            msgs.push(msg);
        }
        msgs
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    transport.send("CHAN_A", b"aaa").await.unwrap();
    transport.send("CHAN_B", b"bbb").await.unwrap();
    transport.send("CHAN_C", b"ccc").await.unwrap();

    let msgs = tokio::time::timeout(TEST_TIMEOUT, recv_handle)
        .await
        .expect("timeout")
        .expect("task panicked");

    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].channel, "CHAN_A");
    assert_eq!(msgs[0].data, b"aaa");
    assert_eq!(msgs[1].channel, "CHAN_B");
    assert_eq!(msgs[1].data, b"bbb");
    assert_eq!(msgs[2].channel, "CHAN_C");
    assert_eq!(msgs[2].data, b"ccc");
}

#[tokio::test]
async fn test_sequence_numbers_increment() {
    let port = unique_port();
    let transport = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());

    let t = transport.clone();
    let recv_handle = tokio::spawn(async move {
        let mut reassembler = new_reassembler();
        let mut msgs = Vec::new();
        for _ in 0..5 {
            let msg = t.recv(&mut reassembler).await.unwrap();
            msgs.push(msg);
        }
        msgs
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut send_seqs = Vec::new();
    for i in 0u8..5 {
        let seq = transport.send("SEQ_TEST", &[i]).await.unwrap();
        send_seqs.push(seq);
    }

    assert_eq!(send_seqs, vec![0, 1, 2, 3, 4]);

    let msgs = tokio::time::timeout(TEST_TIMEOUT, recv_handle)
        .await
        .expect("timeout")
        .expect("task panicked");

    for (i, msg) in msgs.iter().enumerate() {
        assert_eq!(msg.sequence_number, i as u32);
        assert_eq!(msg.data, &[i as u8]);
    }
}

#[tokio::test]
async fn test_two_transports_same_group() {
    let port = unique_port();
    let transport_a = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());
    let transport_b = Arc::new(LcmTransport::new(test_config(port)).await.unwrap());

    // Receive on transport B.
    let tb = transport_b.clone();
    let recv_handle = tokio::spawn(async move {
        let mut reassembler = new_reassembler();
        tb.recv(&mut reassembler).await
    });

    tokio::time::sleep(Duration::from_millis(20)).await;

    // Send from transport A.
    transport_a.send("CROSS", b"from_a").await.unwrap();

    let msg = tokio::time::timeout(TEST_TIMEOUT, recv_handle)
        .await
        .expect("timeout")
        .expect("task panicked")
        .expect("recv error");

    assert_eq!(msg.channel, "CROSS");
    assert_eq!(msg.data, b"from_a");
}
