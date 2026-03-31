use std::collections::HashSet;
use std::sync::Arc;

use lcm_transport::LcmTransport;
use tokio::sync::RwLock;
use zenoh::{
    sample::Locality,
    Session,
};

use crate::config::{is_allowed, Config};
use crate::mapping::key_expr_to_lcm_channel;

/// Prune the sent-sequences set when it grows too large.
///
/// Since sequence numbers are monotonically increasing, we keep a window of
/// recent numbers and discard everything older. This runs O(n) once per ~5k
/// inserts (amortised O(1) per insert).
pub(crate) fn prune_sent_sequences(seqs: &mut HashSet<u32>, latest_seq: u32) {
    if seqs.len() > 10_000 {
        let cutoff = latest_seq.saturating_sub(5_000);
        seqs.retain(|&s| s >= cutoff);
    }
}

/// Run the Zenoh → LCM bridging task.
///
/// Subscribes to Zenoh key expressions under the configured prefix and sends
/// matching messages to the LCM multicast group.
///
/// `sent_sequences` is a shared set of sequence numbers recently sent to LCM,
/// used for loop prevention: the LCM → Zenoh task checks this set to skip
/// messages that originated from this bridge.
pub async fn run(
    transport: Arc<LcmTransport>,
    zsession: Arc<Session>,
    config: Arc<Config>,
    sent_sequences: Arc<RwLock<HashSet<u32>>>,
) {
    // Subscribe to everything under the key prefix.
    let sub_expr = format!("{}/{}", config.key_prefix, "**");

    tracing::info!("Zenoh → LCM bridge task: subscribing to '{}'", sub_expr);

    // Use Locality::Remote to only receive publications from other Zenoh nodes,
    // not from our own LCM → Zenoh publications within this session.
    let subscriber = match zsession
        .declare_subscriber(&sub_expr)
        .allowed_origin(Locality::Remote)
        .await
    {
        Ok(sub) => sub,
        Err(e) => {
            tracing::error!(
                "Zenoh→LCM: failed to declare subscriber on '{}': {}",
                sub_expr,
                e,
            );
            return;
        }
    };

    tracing::info!("Zenoh → LCM bridge task started");

    loop {
        let sample = match subscriber.recv_async().await {
            Ok(sample) => sample,
            Err(e) => {
                tracing::error!("Zenoh→LCM: subscriber recv error: {}", e);
                break;
            }
        };

        let ke = sample.key_expr();

        // Extract channel name from key expression.
        let channel = match key_expr_to_lcm_channel(ke, &config.key_prefix) {
            Ok(ch) => ch,
            Err(e) => {
                tracing::warn!(
                    "Zenoh→LCM: cannot extract channel from key '{}': {}",
                    ke,
                    e,
                );
                continue;
            }
        };

        // Channel filtering.
        if !is_allowed(channel, &config) {
            tracing::trace!(
                "Zenoh→LCM: channel '{}' not allowed, skipping",
                channel,
            );
            continue;
        }

        let payload: Vec<u8> = sample.payload().to_bytes().to_vec();

        tracing::trace!(
            "Zenoh→LCM: key '{}' → channel '{}' ({} bytes)",
            ke,
            channel,
            payload.len(),
        );

        // Send to LCM multicast group.
        match transport.send(channel, &payload).await {
            Ok(seq) => {
                // Record sequence number for loop prevention.
                let mut seqs = sent_sequences.write().await;
                seqs.insert(seq);
                prune_sent_sequences(&mut seqs, seq);
            }
            Err(e) => {
                tracing::warn!(
                    "Zenoh→LCM: failed to send on channel '{}': {}",
                    channel,
                    e,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prune_below_threshold() {
        let mut seqs: HashSet<u32> = (0..100).collect();
        prune_sent_sequences(&mut seqs, 99);
        // Below 10k threshold — no pruning.
        assert_eq!(seqs.len(), 100);
    }

    #[test]
    fn test_prune_above_threshold() {
        let mut seqs: HashSet<u32> = (0..10_001).collect();
        prune_sent_sequences(&mut seqs, 10_000);
        // Should retain only seq >= 10_000 - 5_000 = 5_000.
        assert!(seqs.len() <= 5_001);
        assert!(seqs.contains(&10_000));
        assert!(seqs.contains(&5_000));
        assert!(!seqs.contains(&4_999));
    }

    #[test]
    fn test_prune_saturating_sub_at_zero() {
        // If latest_seq < 5000, saturating_sub returns 0 — retain all.
        let mut seqs: HashSet<u32> = (0..10_001).collect();
        prune_sent_sequences(&mut seqs, 100);
        // cutoff = 100.saturating_sub(5000) = 0, so all are retained.
        assert_eq!(seqs.len(), 10_001);
    }

    #[test]
    fn test_prune_with_gaps() {
        // Non-contiguous sequence numbers (e.g., only even numbers).
        let mut seqs: HashSet<u32> = (0..20_002).step_by(2).collect();
        assert_eq!(seqs.len(), 10_001);
        let latest = 20_000;
        prune_sent_sequences(&mut seqs, latest);
        // cutoff = 20_000 - 5_000 = 15_000. Only even numbers >= 15_000 retained.
        for &s in &seqs {
            assert!(s >= 15_000);
        }
        assert!(seqs.contains(&20_000));
        assert!(seqs.contains(&15_000));
        assert!(!seqs.contains(&14_998));
    }

    #[test]
    fn test_prune_at_u32_max() {
        let mut seqs: HashSet<u32> = HashSet::new();
        // Insert 10_001 entries near u32::MAX.
        for i in 0..10_001u32 {
            seqs.insert(u32::MAX - i);
        }
        prune_sent_sequences(&mut seqs, u32::MAX);
        // cutoff = u32::MAX - 5000. Should retain ~5001 entries.
        assert!(seqs.len() <= 5_001);
        assert!(seqs.contains(&u32::MAX));
        assert!(seqs.contains(&(u32::MAX - 5_000)));
        assert!(!seqs.contains(&(u32::MAX - 5_001)));
    }
}
