use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Instant;

use crate::protocol;
use crate::types::LcmMessage;

/// Key for fragment reassembly: (sender address, sequence number).
///
/// LCM C uses the same composite key to avoid mixing fragments from
/// different publishers that happen to share the same sequence number.
type FragmentKey = (SocketAddr, u32);

/// State for an in-progress fragment reassembly.
struct FragmentSet {
    channel: String,
    payload_size: u32,
    n_fragments: u16,
    fragments_received: u16,
    buffer: Vec<u8>,
    /// Tracks which fragment numbers have been received to avoid double-counting.
    received_mask: Vec<bool>,
    created_at: Instant,
}

/// Reassembles fragmented LCM messages.
pub struct FragmentReassembler {
    /// In-progress fragment sets, keyed by (sender, sequence number).
    pending: HashMap<FragmentKey, FragmentSet>,
    /// Maximum time to wait for all fragments before discarding.
    timeout: std::time::Duration,
    /// Maximum total payload size allowed (guards against memory exhaustion).
    max_message_size: usize,
}

impl FragmentReassembler {
    pub fn new(timeout: std::time::Duration, max_message_size: usize) -> Self {
        Self {
            pending: HashMap::new(),
            timeout,
            max_message_size,
        }
    }

    /// Process a fragment. Returns `Some(LcmMessage)` when all fragments have arrived.
    ///
    /// `sender` identifies the source of this fragment, used together with the
    /// sequence number to distinguish fragments from different LCM publishers.
    pub fn process(
        &mut self,
        fragment: &protocol::Fragment<'_>,
        sender: SocketAddr,
    ) -> Option<LcmMessage> {
        // Garbage collect timed-out fragment sets.
        self.expire_stale();

        let payload_size = fragment.payload_size as usize;
        if payload_size > self.max_message_size {
            tracing::warn!(
                "LCM fragment set seq={} from {}: payload_size={} exceeds max_message_size={}, dropping",
                fragment.sequence_number,
                sender,
                payload_size,
                self.max_message_size,
            );
            return None;
        }

        let key = (sender, fragment.sequence_number);

        let set = self
            .pending
            .entry(key)
            .or_insert_with(|| {
                let mut buffer = Vec::new();
                buffer.resize(payload_size, 0);
                FragmentSet {
                    channel: String::new(),
                    payload_size: fragment.payload_size,
                    n_fragments: fragment.n_fragments,
                    fragments_received: 0,
                    buffer,
                    received_mask: vec![false; fragment.n_fragments as usize],
                    created_at: Instant::now(),
                }
            });

        // Store channel name from first fragment.
        if fragment.fragment_number == 0 {
            if let Some(channel) = fragment.channel {
                set.channel = channel.to_string();
            }
        }

        // Validate consistency.
        if fragment.payload_size != set.payload_size || fragment.n_fragments != set.n_fragments {
            tracing::warn!(
                "LCM fragment set seq={} from {}: inconsistent metadata, dropping fragment",
                fragment.sequence_number,
                sender,
            );
            return None;
        }

        let frag_idx = fragment.fragment_number as usize;
        if frag_idx >= set.received_mask.len() {
            tracing::warn!(
                "LCM fragment seq={}: fragment_number={} >= n_fragments={}",
                fragment.sequence_number,
                fragment.fragment_number,
                fragment.n_fragments,
            );
            return None;
        }

        // Skip duplicate fragments.
        if set.received_mask[frag_idx] {
            return None;
        }

        // Copy fragment data into the reassembly buffer.
        let offset = fragment.fragment_offset as usize;
        let end = offset + fragment.data.len();
        if end > set.buffer.len() {
            tracing::warn!(
                "LCM fragment seq={}: data overflows payload buffer (offset={}, len={}, buf={})",
                fragment.sequence_number,
                offset,
                fragment.data.len(),
                set.buffer.len(),
            );
            return None;
        }

        set.buffer[offset..end].copy_from_slice(fragment.data);
        set.received_mask[frag_idx] = true;
        set.fragments_received += 1;

        // Check if all fragments have arrived.
        if set.fragments_received == set.n_fragments {
            let set = self.pending.remove(&key).unwrap();
            Some(LcmMessage {
                channel: set.channel,
                sequence_number: fragment.sequence_number,
                data: set.buffer,
            })
        } else {
            None
        }
    }

    /// Remove fragment sets that have exceeded the timeout.
    fn expire_stale(&mut self) {
        let now = Instant::now();
        self.pending.retain(|&(ref sender, seq), set| {
            let age = now.duration_since(set.created_at);
            if age > self.timeout {
                tracing::debug!(
                    "LCM fragment set seq={} from {}: timed out after {:?} ({}/{} fragments received)",
                    seq,
                    sender,
                    age,
                    set.fragments_received,
                    set.n_fragments,
                );
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol;
    use std::net::{Ipv4Addr, SocketAddrV4};

    const SENDER_A: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 5000));
    const SENDER_B: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 5001));

    fn make_fragment<'a>(
        seq: u32,
        payload_size: u32,
        offset: u32,
        frag_num: u16,
        n_frags: u16,
        channel: Option<&'a str>,
        data: &'a [u8],
    ) -> protocol::Fragment<'a> {
        protocol::Fragment {
            sequence_number: seq,
            payload_size,
            fragment_offset: offset,
            fragment_number: frag_num,
            n_fragments: n_frags,
            channel,
            data,
        }
    }

    #[test]
    fn test_reassemble_two_fragments() {
        let mut reassembler =
            FragmentReassembler::new(std::time::Duration::from_secs(1), 4 * 1024 * 1024);

        let data_a = [1u8, 2, 3, 4, 5];
        let data_b = [6u8, 7, 8, 9, 10];

        let frag0 = make_fragment(1, 10, 0, 0, 2, Some("CHAN"), &data_a);
        assert!(reassembler.process(&frag0, SENDER_A).is_none());

        let frag1 = make_fragment(1, 10, 5, 1, 2, None, &data_b);
        let msg = reassembler.process(&frag1, SENDER_A).expect("should reassemble");

        assert_eq!(msg.channel, "CHAN");
        assert_eq!(msg.sequence_number, 1);
        assert_eq!(msg.data, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn test_duplicate_fragment_ignored() {
        let mut reassembler =
            FragmentReassembler::new(std::time::Duration::from_secs(1), 4 * 1024 * 1024);

        let data = [1u8, 2, 3];
        let frag0 = make_fragment(1, 6, 0, 0, 2, Some("CH"), &data);
        assert!(reassembler.process(&frag0, SENDER_A).is_none());
        // Sending the same fragment again should not complete the message.
        assert!(reassembler.process(&frag0, SENDER_A).is_none());
    }

    #[test]
    fn test_oversized_message_rejected() {
        let mut reassembler =
            FragmentReassembler::new(std::time::Duration::from_secs(1), 10);

        let data = [1u8; 5];
        let frag0 = make_fragment(1, 100, 0, 0, 2, Some("BIG"), &data);
        assert!(reassembler.process(&frag0, SENDER_A).is_none());
        // The set should not have been created.
        assert!(reassembler.pending.is_empty());
    }

    #[test]
    fn test_different_senders_same_seqno() {
        let mut reassembler =
            FragmentReassembler::new(std::time::Duration::from_secs(1), 4 * 1024 * 1024);

        // Two senders, both with sequence number 1, different data.
        let data_a0 = [1u8, 2, 3];
        let data_a1 = [4u8, 5, 6];
        let data_b0 = [10u8, 20, 30];
        let data_b1 = [40u8, 50, 60];

        let frag_a0 = make_fragment(1, 6, 0, 0, 2, Some("CH_A"), &data_a0);
        let frag_b0 = make_fragment(1, 6, 0, 0, 2, Some("CH_B"), &data_b0);
        let frag_a1 = make_fragment(1, 6, 3, 1, 2, None, &data_a1);
        let frag_b1 = make_fragment(1, 6, 3, 1, 2, None, &data_b1);

        // Interleave fragments from both senders.
        assert!(reassembler.process(&frag_a0, SENDER_A).is_none());
        assert!(reassembler.process(&frag_b0, SENDER_B).is_none());

        let msg_a = reassembler.process(&frag_a1, SENDER_A).expect("should reassemble A");
        let msg_b = reassembler.process(&frag_b1, SENDER_B).expect("should reassemble B");

        assert_eq!(msg_a.channel, "CH_A");
        assert_eq!(msg_a.data, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(msg_b.channel, "CH_B");
        assert_eq!(msg_b.data, &[10, 20, 30, 40, 50, 60]);
    }
}
