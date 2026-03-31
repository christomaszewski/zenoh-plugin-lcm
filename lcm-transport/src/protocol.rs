/// LCM short message magic number: "LC02" in ASCII.
pub const MAGIC_SHORT: u32 = 0x4c433032;
/// LCM fragment message magic number: "LC03" in ASCII.
pub const MAGIC_FRAGMENT: u32 = 0x4c433033;

/// Minimum size of a short message header (magic + seq + channel_len).
pub const SHORT_HEADER_SIZE: usize = 4 + 4;
/// Size of a fragment header.
pub const FRAGMENT_HEADER_SIZE: usize = 20;

/// A parsed short (non-fragmented) LCM message.
#[derive(Debug)]
pub struct ShortMessage<'a> {
    pub sequence_number: u32,
    pub channel: &'a str,
    pub data: &'a [u8],
}

/// A parsed LCM fragment.
#[derive(Debug)]
pub struct Fragment<'a> {
    pub sequence_number: u32,
    pub payload_size: u32,
    pub fragment_offset: u32,
    pub fragment_number: u16,
    pub n_fragments: u16,
    /// Channel name (only present in first fragment, fragment_number == 0).
    pub channel: Option<&'a str>,
    /// Fragment data payload.
    pub data: &'a [u8],
}

/// Result of parsing an LCM UDP datagram.
#[derive(Debug)]
pub enum Packet<'a> {
    Short(ShortMessage<'a>),
    Fragment(Fragment<'a>),
}

/// Parse a raw UDP datagram into an LCM packet.
///
/// Returns `None` if the datagram is too small or has an unrecognized magic number.
pub fn parse_datagram(buf: &[u8]) -> Option<Packet<'_>> {
    if buf.len() < 4 {
        return None;
    }

    let magic = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

    match magic {
        MAGIC_SHORT => parse_short_message(buf),
        MAGIC_FRAGMENT => parse_fragment(buf),
        _ => {
            tracing::trace!("Unknown LCM magic number: 0x{:08x}", magic);
            None
        }
    }
}

fn parse_short_message(buf: &[u8]) -> Option<Packet<'_>> {
    // magic(4) + sequence_number(4) = 8 bytes minimum before channel name
    if buf.len() < SHORT_HEADER_SIZE {
        return None;
    }

    let sequence_number = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

    // The channel name starts at offset 8 and is null-terminated.
    let channel_start = 8;
    let channel_and_data = &buf[channel_start..];

    // Find the null terminator for the channel name.
    let null_pos = channel_and_data.iter().position(|&b| b == 0)?;
    let channel = std::str::from_utf8(&channel_and_data[..null_pos]).ok()?;

    let data_start = null_pos + 1;
    let data = &channel_and_data[data_start..];

    Some(Packet::Short(ShortMessage {
        sequence_number,
        channel,
        data,
    }))
}

fn parse_fragment(buf: &[u8]) -> Option<Packet<'_>> {
    if buf.len() < FRAGMENT_HEADER_SIZE {
        return None;
    }

    let sequence_number = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let payload_size = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let fragment_offset = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
    let fragment_number = u16::from_be_bytes([buf[16], buf[17]]);
    let n_fragments = u16::from_be_bytes([buf[18], buf[19]]);

    let rest = &buf[FRAGMENT_HEADER_SIZE..];

    let (channel, data) = if fragment_number == 0 {
        // First fragment contains the channel name (null-terminated).
        let null_pos = rest.iter().position(|&b| b == 0)?;
        let channel = std::str::from_utf8(&rest[..null_pos]).ok()?;
        (Some(channel), &rest[null_pos + 1..])
    } else {
        (None, rest)
    };

    Some(Packet::Fragment(Fragment {
        sequence_number,
        payload_size,
        fragment_offset,
        fragment_number,
        n_fragments,
        channel,
        data,
    }))
}

/// Encode a short (non-fragmented) LCM message into a buffer.
///
/// Returns the number of bytes written.
pub fn encode_short_message(
    buf: &mut Vec<u8>,
    sequence_number: u32,
    channel: &str,
    data: &[u8],
) {
    buf.clear();
    // magic
    buf.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
    // sequence_number
    buf.extend_from_slice(&sequence_number.to_be_bytes());
    // channel name (null-terminated)
    buf.extend_from_slice(channel.as_bytes());
    buf.push(0);
    // payload
    buf.extend_from_slice(data);
}

/// Encode a single fragment header and its data into a buffer.
///
/// If `fragment_number == 0`, the channel name is included after the header.
pub fn encode_fragment(
    buf: &mut Vec<u8>,
    sequence_number: u32,
    payload_size: u32,
    fragment_offset: u32,
    fragment_number: u16,
    n_fragments: u16,
    channel: Option<&str>,
    data: &[u8],
) {
    buf.clear();
    // magic
    buf.extend_from_slice(&MAGIC_FRAGMENT.to_be_bytes());
    // sequence_number
    buf.extend_from_slice(&sequence_number.to_be_bytes());
    // payload_size
    buf.extend_from_slice(&payload_size.to_be_bytes());
    // fragment_offset
    buf.extend_from_slice(&fragment_offset.to_be_bytes());
    // fragment_number
    buf.extend_from_slice(&fragment_number.to_be_bytes());
    // n_fragments
    buf.extend_from_slice(&n_fragments.to_be_bytes());
    // channel name (only in first fragment)
    if let Some(channel) = channel {
        buf.extend_from_slice(channel.as_bytes());
        buf.push(0);
    }
    // data
    buf.extend_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_short_message() {
        let mut buf = Vec::new();
        encode_short_message(&mut buf, 42, "TEST_CHANNEL", &[1, 2, 3, 4]);

        let packet = parse_datagram(&buf).expect("should parse");
        match packet {
            Packet::Short(msg) => {
                assert_eq!(msg.sequence_number, 42);
                assert_eq!(msg.channel, "TEST_CHANNEL");
                assert_eq!(msg.data, &[1, 2, 3, 4]);
            }
            Packet::Fragment(_) => panic!("expected short message"),
        }
    }

    #[test]
    fn test_roundtrip_fragment() {
        let mut buf = Vec::new();
        encode_fragment(&mut buf, 99, 1000, 0, 0, 3, Some("FRAG_CHAN"), &[5, 6, 7]);

        let packet = parse_datagram(&buf).expect("should parse");
        match packet {
            Packet::Fragment(frag) => {
                assert_eq!(frag.sequence_number, 99);
                assert_eq!(frag.payload_size, 1000);
                assert_eq!(frag.fragment_offset, 0);
                assert_eq!(frag.fragment_number, 0);
                assert_eq!(frag.n_fragments, 3);
                assert_eq!(frag.channel, Some("FRAG_CHAN"));
                assert_eq!(frag.data, &[5, 6, 7]);
            }
            Packet::Short(_) => panic!("expected fragment"),
        }
    }

    #[test]
    fn test_subsequent_fragment_no_channel() {
        let mut buf = Vec::new();
        encode_fragment(&mut buf, 99, 1000, 500, 1, 3, None, &[8, 9]);

        let packet = parse_datagram(&buf).expect("should parse");
        match packet {
            Packet::Fragment(frag) => {
                assert_eq!(frag.fragment_number, 1);
                assert_eq!(frag.channel, None);
                assert_eq!(frag.fragment_offset, 500);
                assert_eq!(frag.data, &[8, 9]);
            }
            Packet::Short(_) => panic!("expected fragment"),
        }
    }

    #[test]
    fn test_parse_too_small() {
        assert!(parse_datagram(&[0, 1, 2]).is_none());
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_datagram(&[]).is_none());
    }

    #[test]
    fn test_parse_unknown_magic() {
        assert!(parse_datagram(&[0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    // --- Malformed packet tests ---

    #[test]
    fn test_short_message_no_null_terminator() {
        // Valid magic + seqno, but channel bytes without a null terminator.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(b"CHAN_NO_NULL");
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_short_message_invalid_utf8_channel() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&[0xFF, 0xFE, 0x00]); // Invalid UTF-8 + null
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_short_message_header_only() {
        // Exactly SHORT_HEADER_SIZE bytes — valid header but no channel.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        // No channel data at all, just header — null terminator can't be found.
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_short_message_empty_data() {
        // Channel immediately followed by null, then no data.
        let mut buf = Vec::new();
        encode_short_message(&mut buf, 0, "CH", &[]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Short(msg) => {
                assert_eq!(msg.channel, "CH");
                assert!(msg.data.is_empty());
            }
            _ => panic!("expected short"),
        }
    }

    #[test]
    fn test_fragment_header_truncated() {
        // Less than FRAGMENT_HEADER_SIZE bytes with fragment magic.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_FRAGMENT.to_be_bytes());
        buf.extend_from_slice(&[0u8; 10]); // Only 14 bytes total, need 20.
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_fragment_first_no_null_terminator() {
        // First fragment (frag_num=0) but no null terminator for channel.
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_FRAGMENT.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // seqno
        buf.extend_from_slice(&100u32.to_be_bytes()); // payload_size
        buf.extend_from_slice(&0u32.to_be_bytes()); // offset
        buf.extend_from_slice(&0u16.to_be_bytes()); // frag_num = 0
        buf.extend_from_slice(&2u16.to_be_bytes()); // n_frags
        buf.extend_from_slice(b"CHAN_NO_NULL");
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_fragment_first_invalid_utf8() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC_FRAGMENT.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&100u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&2u16.to_be_bytes());
        buf.extend_from_slice(&[0xFF, 0xFE, 0x00]); // Invalid UTF-8 + null
        assert!(parse_datagram(&buf).is_none());
    }

    #[test]
    fn test_fragment_non_first_no_channel() {
        // Non-first fragment has no channel field — should parse fine.
        let mut buf = Vec::new();
        encode_fragment(&mut buf, 1, 100, 50, 1, 3, None, &[10, 20, 30]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Fragment(frag) => {
                assert_eq!(frag.fragment_number, 1);
                assert!(frag.channel.is_none());
                assert_eq!(frag.data, &[10, 20, 30]);
            }
            _ => panic!("expected fragment"),
        }
    }

    // --- Protocol edge case tests ---

    #[test]
    fn test_short_message_max_sequence_number() {
        let mut buf = Vec::new();
        encode_short_message(&mut buf, u32::MAX, "CH", &[1]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Short(msg) => assert_eq!(msg.sequence_number, u32::MAX),
            _ => panic!("expected short"),
        }
    }

    #[test]
    fn test_fragment_max_sequence_number() {
        let mut buf = Vec::new();
        encode_fragment(&mut buf, u32::MAX, 10, 0, 0, 1, Some("CH"), &[1]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Fragment(frag) => assert_eq!(frag.sequence_number, u32::MAX),
            _ => panic!("expected fragment"),
        }
    }

    #[test]
    fn test_short_message_long_channel() {
        let long_channel: String = "A".repeat(500);
        let mut buf = Vec::new();
        encode_short_message(&mut buf, 0, &long_channel, &[1, 2, 3]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Short(msg) => assert_eq!(msg.channel, long_channel),
            _ => panic!("expected short"),
        }
    }

    #[test]
    fn test_short_message_large_payload() {
        let payload: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let mut buf = Vec::new();
        encode_short_message(&mut buf, 7, "BIG", &payload);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Short(msg) => {
                assert_eq!(msg.data.len(), 10_000);
                assert_eq!(msg.data, payload.as_slice());
            }
            _ => panic!("expected short"),
        }
    }

    #[test]
    fn test_fragment_empty_data() {
        // Non-first fragment with zero-length data.
        let mut buf = Vec::new();
        encode_fragment(&mut buf, 1, 10, 5, 1, 2, None, &[]);
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Fragment(frag) => {
                assert!(frag.data.is_empty());
                assert_eq!(frag.fragment_offset, 5);
            }
            _ => panic!("expected fragment"),
        }
    }

    #[test]
    fn test_fragment_max_values() {
        let mut buf = Vec::new();
        encode_fragment(
            &mut buf,
            u32::MAX,
            u32::MAX,
            u32::MAX,
            u16::MAX,
            u16::MAX,
            None,
            &[0xFF],
        );
        let packet = parse_datagram(&buf).unwrap();
        match packet {
            Packet::Fragment(frag) => {
                assert_eq!(frag.sequence_number, u32::MAX);
                assert_eq!(frag.payload_size, u32::MAX);
                assert_eq!(frag.fragment_offset, u32::MAX);
                assert_eq!(frag.fragment_number, u16::MAX);
                assert_eq!(frag.n_fragments, u16::MAX);
            }
            _ => panic!("expected fragment"),
        }
    }

    #[test]
    fn test_short_magic_with_fragment_size() {
        // Short message magic but only enough bytes for the magic — no seqno.
        let buf = MAGIC_SHORT.to_be_bytes().to_vec();
        assert!(parse_datagram(&buf).is_none());
    }
}
