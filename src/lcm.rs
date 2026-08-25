use anyhow::{bail, Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MAGIC_SHORT: u32 = 0x4c43_3032;
const MAGIC_LONG: u32 = 0x4c43_3033;
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const FRAGMENT_TIMEOUT: Duration = Duration::from_secs(2);
const RECEIVE_BUFFER_BYTES: usize = 16 * 1024 * 1024;

pub const DEFAULT_URL: &str = "udpm://239.255.76.67:7667?ttl=0";

#[derive(Clone, Copy)]
pub struct LcmUrl {
    pub group: Ipv4Addr,
    pub port: u16,
    pub ttl: u32,
}

pub fn parse_url(url: &str) -> Result<LcmUrl> {
    let rest = url
        .strip_prefix("udpm://")
        .with_context(|| format!("only udpm:// lcm urls are supported, got {url}"))?;
    let (address, query) = match rest.split_once('?') {
        Some((address, query)) => (address, Some(query)),
        None => (rest, None),
    };
    let (host, port) = address
        .split_once(':')
        .with_context(|| format!("lcm url is missing a port: {url}"))?;
    let mut ttl = 0;
    if let Some(query) = query {
        for parameter in query.split('&') {
            if let Some(value) = parameter.strip_prefix("ttl=") {
                ttl = value.parse().unwrap_or(0);
            }
        }
    }
    Ok(LcmUrl {
        group: host.parse()?,
        port: port.parse()?,
        ttl,
    })
}

pub struct LcmTransport {
    socket: UdpSocket,
    destination: SocketAddrV4,
    sequence: AtomicU32,
}

impl LcmTransport {
    pub fn new(url: LcmUrl) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
        socket.set_multicast_ttl_v4(url.ttl)?;
        socket.set_multicast_loop_v4(true)?;
        Ok(LcmTransport {
            socket,
            destination: SocketAddrV4::new(url.group, url.port),
            sequence: AtomicU32::new(0),
        })
    }

    pub fn publish(&self, channel: &str, payload: &[u8]) -> Result<()> {
        if channel.len() + payload.len() + 9 > 65_000 {
            bail!("publishing fragmented lcm messages is not supported");
        }
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut packet = Vec::with_capacity(channel.len() + payload.len() + 9);
        packet.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(channel.as_bytes());
        packet.push(0);
        packet.extend_from_slice(payload);
        self.socket.send_to(&packet, self.destination)?;
        Ok(())
    }
}

struct PartialMessage {
    channel: Option<String>,
    payload: Vec<u8>,
    received_bytes: usize,
    last_seen: Instant,
}

pub enum Incoming<'a> {
    Message {
        channel: &'a str,
        payload: &'a [u8],
    },
    /// A message on a topic nobody is watching: counted for discovery, never reassembled.
    Skipped {
        channel: &'a str,
        bytes: usize,
    },
}

/// Receives every message on the multicast group and hands each one to `sink`.
///
/// `wants_payload` is consulted before a fragmented message is reassembled, so
/// image topics nobody is watching cost one hash lookup instead of a 400 KB copy.
/// `on_listening` fires once the group has actually been joined, which is the
/// only point at which this process can hear anything.
pub fn run_receiver<S, W, L>(url: LcmUrl, sink: S, wants_payload: W, on_listening: L) -> Result<()>
where
    S: Fn(Incoming),
    W: Fn(&str) -> bool,
    L: Fn(),
{
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, url.port).into())?;
    socket
        .join_multicast_v4(&url.group, &Ipv4Addr::UNSPECIFIED)
        .with_context(|| format!("joining lcm multicast group {}", url.group))?;
    let _ = socket.set_recv_buffer_size(RECEIVE_BUFFER_BYTES);
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    on_listening();

    let mut buffer = [MaybeUninit::<u8>::uninit(); 65_536];
    let mut partials: HashMap<(SocketAddr, u32), PartialMessage> = HashMap::new();
    let mut last_sweep = Instant::now();

    loop {
        let (length, source) = match socket.recv_from(&mut buffer) {
            Ok(result) => result,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                sweep(&mut partials, &mut last_sweep);
                continue;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        let packet = unsafe { &*(&buffer[..length] as *const [MaybeUninit<u8>] as *const [u8]) };
        let source = source.as_socket().unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        handle_packet(packet, source, &sink, &wants_payload, &mut partials);
        sweep(&mut partials, &mut last_sweep);
    }
}

fn handle_packet<S, W>(
    packet: &[u8],
    source: SocketAddr,
    sink: &S,
    wants_payload: &W,
    partials: &mut HashMap<(SocketAddr, u32), PartialMessage>,
) where
    S: Fn(Incoming),
    W: Fn(&str) -> bool,
{
    if packet.len() < 8 {
        return;
    }
    let magic = u32::from_be_bytes(packet[0..4].try_into().unwrap());
    if magic == MAGIC_SHORT {
        let Some((channel, payload)) = split_channel(&packet[8..]) else {
            return;
        };
        sink(Incoming::Message { channel, payload });
        return;
    }
    if magic != MAGIC_LONG || packet.len() < 20 {
        return;
    }

    let sequence = u32::from_be_bytes(packet[4..8].try_into().unwrap());
    let message_size = u32::from_be_bytes(packet[8..12].try_into().unwrap()) as usize;
    let fragment_offset = u32::from_be_bytes(packet[12..16].try_into().unwrap()) as usize;
    let fragment_number = u16::from_be_bytes(packet[16..18].try_into().unwrap());
    let fragment_count = u16::from_be_bytes(packet[18..20].try_into().unwrap());
    if message_size == 0 || message_size > MAX_MESSAGE_BYTES || fragment_count == 0 {
        return;
    }

    let body = &packet[20..];
    let (channel, chunk) = if fragment_number == 0 {
        match split_channel(body) {
            Some((channel, chunk)) => (Some(channel.to_owned()), chunk),
            None => return,
        }
    } else {
        (None, body)
    };

    // Fragment 0 carries the channel name, so a message whose first fragment is
    // lost can never be attributed and is dropped by the sweep.
    if let Some(channel) = channel.as_deref() {
        if !wants_payload(channel) {
            sink(Incoming::Skipped {
                channel,
                bytes: message_size,
            });
            partials.remove(&(source, sequence));
            return;
        }
    }

    let key = (source, sequence);
    let partial = partials.entry(key).or_insert_with(|| PartialMessage {
        channel: None,
        payload: vec![0; message_size],
        received_bytes: 0,
        last_seen: Instant::now(),
    });
    partial.last_seen = Instant::now();
    if channel.is_some() {
        partial.channel = channel;
    }
    if partial.payload.len() != message_size || fragment_offset + chunk.len() > message_size {
        partials.remove(&key);
        return;
    }
    partial.payload[fragment_offset..fragment_offset + chunk.len()].copy_from_slice(chunk);
    partial.received_bytes += chunk.len();

    if partial.received_bytes >= message_size {
        let partial = partials.remove(&key).unwrap();
        if let Some(channel) = partial.channel {
            sink(Incoming::Message {
                channel: &channel,
                payload: &partial.payload,
            });
        }
    }
}

fn split_channel(body: &[u8]) -> Option<(&str, &[u8])> {
    let terminator = body.iter().position(|byte| *byte == 0)?;
    let channel = std::str::from_utf8(&body[..terminator]).ok()?;
    Some((channel, &body[terminator + 1..]))
}

fn sweep(partials: &mut HashMap<(SocketAddr, u32), PartialMessage>, last_sweep: &mut Instant) {
    if last_sweep.elapsed() < FRAGMENT_TIMEOUT {
        return;
    }
    *last_sweep = Instant::now();
    partials.retain(|_, partial| partial.last_seen.elapsed() < FRAGMENT_TIMEOUT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn parses_the_dimos_default_url() {
        let url = parse_url(DEFAULT_URL).unwrap();
        assert_eq!(url.group, Ipv4Addr::new(239, 255, 76, 67));
        assert_eq!(url.port, 7667);
        assert_eq!(url.ttl, 0);
    }

    #[test]
    fn reassembles_a_fragmented_message() {
        let channel = "/image#sensor_msgs.Image";
        let payload: Vec<u8> = (0..5000u32).map(|index| index as u8).collect();
        let mut partials = HashMap::new();
        let received = RefCell::new(Vec::new());
        let sink = |incoming: Incoming| {
            if let Incoming::Message { channel, payload } = incoming {
                received.borrow_mut().push((channel.to_owned(), payload.to_vec()));
            }
        };
        let source = SocketAddr::from(([127, 0, 0, 1], 7667));

        for (index, chunk) in payload.chunks(2000).enumerate() {
            let mut packet = Vec::new();
            packet.extend_from_slice(&MAGIC_LONG.to_be_bytes());
            packet.extend_from_slice(&7u32.to_be_bytes());
            packet.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            packet.extend_from_slice(&((index * 2000) as u32).to_be_bytes());
            packet.extend_from_slice(&(index as u16).to_be_bytes());
            packet.extend_from_slice(&3u16.to_be_bytes());
            if index == 0 {
                packet.extend_from_slice(channel.as_bytes());
                packet.push(0);
            }
            packet.extend_from_slice(chunk);
            handle_packet(&packet, source, &sink, &|_| true, &mut partials);
        }

        let received = received.into_inner();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].0, channel);
        assert_eq!(received[0].1, payload);
        assert!(partials.is_empty());
    }

    #[test]
    fn unwanted_fragmented_topics_are_announced_but_not_buffered() {
        let mut partials = HashMap::new();
        let received = RefCell::new(Vec::new());
        let sink = |incoming: Incoming| match incoming {
            Incoming::Skipped { channel, bytes } => {
                received.borrow_mut().push((channel.to_owned(), bytes));
            }
            Incoming::Message { .. } => panic!("unwatched topic was reassembled"),
        };
        let mut packet = Vec::new();
        packet.extend_from_slice(&MAGIC_LONG.to_be_bytes());
        packet.extend_from_slice(&1u32.to_be_bytes());
        packet.extend_from_slice(&4000u32.to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&2u16.to_be_bytes());
        packet.extend_from_slice(b"/depth#sensor_msgs.Image\0");
        packet.extend_from_slice(&[0u8; 2000]);
        handle_packet(
            &packet,
            SocketAddr::from(([127, 0, 0, 1], 7667)),
            &sink,
            &|_| false,
            &mut partials,
        );

        assert_eq!(received.into_inner(), vec![("/depth#sensor_msgs.Image".to_owned(), 4000)]);
        assert!(partials.is_empty());
    }

    #[test]
    fn short_messages_pass_straight_through() {
        let mut partials = HashMap::new();
        let received = RefCell::new(Vec::new());
        let sink = |incoming: Incoming| {
            if let Incoming::Message { channel, payload } = incoming {
                received.borrow_mut().push((channel.to_owned(), payload.to_vec()));
            }
        };
        let mut packet = Vec::new();
        packet.extend_from_slice(&MAGIC_SHORT.to_be_bytes());
        packet.extend_from_slice(&3u32.to_be_bytes());
        packet.extend_from_slice(b"/tele_cmd_vel#geometry_msgs.Twist\0");
        packet.extend_from_slice(&[9, 9, 9]);
        handle_packet(
            &packet,
            SocketAddr::from(([127, 0, 0, 1], 7667)),
            &sink,
            &|_| true,
            &mut partials,
        );

        let received = received.into_inner();
        assert_eq!(received[0].0, "/tele_cmd_vel#geometry_msgs.Twist");
        assert_eq!(received[0].1, vec![9, 9, 9]);
    }
}
