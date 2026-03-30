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
                // Bulk prune when the set grows too large. Since sequence
                // numbers are monotonically increasing, discard everything
                // older than a recent window. This runs O(n) once per ~5k
                // inserts rather than on every insert.
                if seqs.len() > 10_000 {
                    let cutoff = seq.saturating_sub(5_000);
                    seqs.retain(|&s| s >= cutoff);
                }
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
