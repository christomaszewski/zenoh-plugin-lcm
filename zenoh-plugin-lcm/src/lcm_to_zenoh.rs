use std::collections::HashSet;
use std::sync::Arc;

use lcm_transport::{FragmentReassembler, LcmTransport};
use tokio::sync::RwLock;
use zenoh::{
    bytes::ZBytes,
    sample::Locality,
    Session,
};

use crate::config::{is_allowed, Config};
use crate::mapping::lcm_channel_to_key_expr;

/// Run the LCM → Zenoh bridging task.
///
/// Receives LCM messages from multicast and publishes them to Zenoh.
///
/// `sent_sequences` is a shared set of sequence numbers recently sent by the
/// Zenoh → LCM direction, used for loop prevention on the LCM side.
pub async fn run(
    transport: Arc<LcmTransport>,
    zsession: Arc<Session>,
    config: Arc<Config>,
    sent_sequences: Arc<RwLock<HashSet<u32>>>,
) {
    let mut reassembler = FragmentReassembler::new(
        std::time::Duration::from_secs(1),
        config.max_message_size,
    );

    tracing::info!("LCM → Zenoh bridge task started");

    loop {
        let msg = match transport.recv(&mut reassembler).await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::error!("Error receiving LCM message: {}", e);
                continue;
            }
        };

        // Loop prevention: skip messages we sent to LCM (Zenoh → LCM direction).
        {
            let mut seqs = sent_sequences.write().await;
            if seqs.remove(&msg.sequence_number) {
                tracing::trace!(
                    "LCM→Zenoh: skipping loopback message seq={} on channel '{}'",
                    msg.sequence_number,
                    msg.channel,
                );
                continue;
            }
        }

        // Channel filtering.
        if !is_allowed(&msg.channel, &config) {
            tracing::trace!(
                "LCM→Zenoh: channel '{}' not allowed, skipping",
                msg.channel,
            );
            continue;
        }

        // Map channel to Zenoh key expression.
        let ke = match lcm_channel_to_key_expr(&msg.channel, &config.key_prefix) {
            Ok(ke) => ke,
            Err(e) => {
                tracing::warn!(
                    "LCM→Zenoh: cannot map channel '{}' to key expression: {}",
                    msg.channel,
                    e,
                );
                continue;
            }
        };

        tracing::trace!(
            "LCM→Zenoh: channel '{}' seq={} → key '{}'  ({} bytes)",
            msg.channel,
            msg.sequence_number,
            ke,
            msg.data.len(),
        );

        // Publish to Zenoh. Use Locality::Remote destination to avoid the
        // Zenoh → LCM subscriber in this same session from picking it up.
        let put = zsession
            .put(&ke, ZBytes::from(msg.data))
            .allowed_destination(Locality::Remote);

        if let Err(e) = put.await {
            tracing::warn!(
                "LCM→Zenoh: failed to publish on '{}': {}",
                ke,
                e,
            );
        }
    }
}
