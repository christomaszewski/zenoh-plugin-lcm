use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;

use crate::fragment::FragmentReassembler;
use crate::protocol::{self, Packet};
use crate::types::{LcmMessage, LcmUrl};

/// Default maximum UDP datagram size for receiving.
const MAX_DATAGRAM_SIZE: usize = 65536;
/// Default MTU for fragmenting outgoing messages.
/// Conservative value to stay well under typical Ethernet MTU minus IP/UDP headers.
const DEFAULT_FRAGMENT_MTU: usize = 1400;
/// Default fragment reassembly timeout.
const DEFAULT_FRAGMENT_TIMEOUT: Duration = Duration::from_secs(1);
/// Default maximum reassembled message size (4 MB).
const DEFAULT_MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// Configuration for the LCM transport.
#[derive(Debug, Clone)]
pub struct LcmTransportConfig {
    pub lcm_url: LcmUrl,
    pub network_interface: Option<Ipv4Addr>,
    pub fragment_mtu: usize,
    pub fragment_timeout: Duration,
    pub max_message_size: usize,
}

impl Default for LcmTransportConfig {
    fn default() -> Self {
        Self {
            lcm_url: LcmUrl::default(),
            network_interface: None,
            fragment_mtu: DEFAULT_FRAGMENT_MTU,
            fragment_timeout: DEFAULT_FRAGMENT_TIMEOUT,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }
}

/// Async LCM UDP multicast transport.
///
/// Provides send/receive of complete LCM messages over UDP multicast,
/// handling fragmentation and reassembly transparently.
pub struct LcmTransport {
    socket: Arc<UdpSocket>,
    multicast_addr: SocketAddr,
    fragment_mtu: usize,
    sequence_counter: AtomicU32,
}

impl LcmTransport {
    /// Create a new LCM transport from a URL string.
    pub async fn from_url(url: &str) -> io::Result<Self> {
        let lcm_url = LcmUrl::parse(url).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let config = LcmTransportConfig {
            lcm_url,
            ..Default::default()
        };
        Self::new(config).await
    }

    /// Create a new LCM transport with full configuration.
    pub async fn new(config: LcmTransportConfig) -> io::Result<Self> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.lcm_url.port);
        let interface = config.network_interface.unwrap_or(Ipv4Addr::UNSPECIFIED);

        // Use socket2 for fine-grained socket options before converting to tokio.
        let socket2 = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket2.set_reuse_address(true)?;
        // On Linux/macOS, SO_REUSEPORT allows multiple processes to bind the same port.
        #[cfg(unix)]
        socket2.set_reuse_port(true)?;
        socket2.set_nonblocking(true)?;
        socket2.bind(&socket2::SockAddr::from(bind_addr))?;

        // Set receive buffer size if specified.
        if let Some(recv_buf_size) = config.lcm_url.recv_buf_size {
            socket2.set_recv_buffer_size(recv_buf_size)?;
        }

        // Set multicast TTL.
        socket2.set_multicast_ttl_v4(config.lcm_url.ttl)?;
        // Enable multicast loopback so local LCM applications can receive
        // messages sent by this bridge. Loop prevention is handled at the
        // bridge level via sequence number tracking, not at the socket level.
        // This matches the LCM C implementation which sets IP_MULTICAST_LOOP=1.
        socket2.set_multicast_loop_v4(true)?;

        // Join multicast group.
        socket2.join_multicast_v4(&config.lcm_url.multicast_group, &interface)?;

        let std_socket: std::net::UdpSocket = socket2.into();
        let socket = UdpSocket::from_std(std_socket)?;

        let multicast_addr = SocketAddr::V4(SocketAddrV4::new(
            config.lcm_url.multicast_group,
            config.lcm_url.port,
        ));

        Ok(Self {
            socket: Arc::new(socket),
            multicast_addr,
            fragment_mtu: config.fragment_mtu,
            sequence_counter: AtomicU32::new(0),
        })
    }

    /// Receive the next complete LCM message from the multicast group.
    ///
    /// Handles fragment reassembly transparently. Blocks until a complete message is available.
    pub async fn recv(
        &self,
        reassembler: &mut FragmentReassembler,
    ) -> io::Result<LcmMessage> {
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];

        loop {
            let (len, src) = self.socket.recv_from(&mut buf).await?;
            let datagram = &buf[..len];

            match protocol::parse_datagram(datagram) {
                Some(Packet::Short(msg)) => {
                    return Ok(LcmMessage {
                        channel: msg.channel.to_string(),
                        sequence_number: msg.sequence_number,
                        data: msg.data.to_vec(),
                    });
                }
                Some(Packet::Fragment(frag)) => {
                    if let Some(msg) = reassembler.process(&frag, src) {
                        return Ok(msg);
                    }
                    // Incomplete fragment set, continue receiving.
                }
                None => {
                    // Unrecognized datagram, skip.
                    continue;
                }
            }
        }
    }

    /// Send an LCM message to the multicast group.
    ///
    /// Automatically fragments if the message exceeds the MTU.
    /// Returns the sequence number used.
    pub async fn send(&self, channel: &str, data: &[u8]) -> io::Result<u32> {
        let seq = self.sequence_counter.fetch_add(1, Ordering::Relaxed);

        // Calculate the total short message size.
        let short_size = 4 + 4 + channel.len() + 1 + data.len(); // magic + seq + channel\0 + data

        if short_size <= self.fragment_mtu {
            // Fits in a single datagram.
            let mut buf = Vec::with_capacity(short_size);
            protocol::encode_short_message(&mut buf, seq, channel, data);
            self.socket.send_to(&buf, self.multicast_addr).await?;
        } else {
            // Must fragment.
            self.send_fragmented(seq, channel, data).await?;
        }

        Ok(seq)
    }

    async fn send_fragmented(
        &self,
        sequence_number: u32,
        channel: &str,
        data: &[u8],
    ) -> io::Result<()> {
        let payload_size = data.len() as u32;

        // Calculate the data capacity per fragment.
        // First fragment: header(20) + channel\0 + data
        // Subsequent fragments: header(20) + data
        let first_frag_overhead = protocol::FRAGMENT_HEADER_SIZE + channel.len() + 1;
        let first_frag_capacity = self.fragment_mtu.saturating_sub(first_frag_overhead);
        let subsequent_frag_capacity =
            self.fragment_mtu.saturating_sub(protocol::FRAGMENT_HEADER_SIZE);

        if first_frag_capacity == 0 || subsequent_frag_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Fragment MTU too small for channel name",
            ));
        }

        // Calculate total number of fragments.
        let remaining_after_first = data.len().saturating_sub(first_frag_capacity);
        let n_fragments = 1
            + remaining_after_first
                .checked_add(subsequent_frag_capacity - 1)
                .map(|v| v / subsequent_frag_capacity)
                .unwrap_or(0);
        let n_fragments = n_fragments as u16;

        let mut buf = Vec::with_capacity(self.fragment_mtu);
        let mut offset: usize = 0;

        for frag_num in 0..n_fragments {
            let (chunk_data, chan) = if frag_num == 0 {
                let end = std::cmp::min(first_frag_capacity, data.len());
                (&data[..end], Some(channel))
            } else {
                let end = std::cmp::min(offset + subsequent_frag_capacity, data.len());
                (&data[offset..end], None)
            };

            protocol::encode_fragment(
                &mut buf,
                sequence_number,
                payload_size,
                offset as u32,
                frag_num,
                n_fragments,
                chan,
                chunk_data,
            );

            self.socket.send_to(&buf, self.multicast_addr).await?;
            offset += chunk_data.len();
        }

        Ok(())
    }

    /// Get a reference to the underlying socket.
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    /// Get the multicast address this transport sends to.
    pub fn multicast_addr(&self) -> SocketAddr {
        self.multicast_addr
    }
}
