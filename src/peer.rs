use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Serialize, Deserialize)]
pub struct Handshake {
    /// length of the protocol string (BitTorrent protocol) which is 19 (1 byte)
    pub length: u8,

    /// the string BitTorrent protocol (19 bytes)
    pub string: [u8; 19],

    /// eight reserved bytes, which are all set to zero (8 bytes)
    pub reserved: [u8; 8],

    /// sha1 info-hash (20 bytes) (NOT the hexadecimal representation, which is 40 bytes long)
    pub info_hash: [u8; 20],

    /// peer id (20 bytes) (generate 20 random byte values)
    pub peer_id: [u8; 20],
}
#[derive(Debug, Clone)]
pub struct PeerMessage {
    pub length: [u8; 4],
    pub kind: u8,
    pub content: Vec<u8>,
}
impl PeerMessage {
    pub fn new() -> Self {
        PeerMessage {
            length: [0; 4],
            kind: 0,
            content: vec![],
        }
    }
    pub fn new_request(piece_i: usize, offset: usize, block_size: usize) -> Self {
        let content = [
            (piece_i as u32).to_be_bytes(),
            (offset as u32).to_be_bytes(),
            (block_size as u32).to_be_bytes(),
        ]
        .concat();
        let length:[u8; 4] = ((content.len() + 1) as u32).to_be_bytes();

        PeerMessage {
            length,
            kind: 6,
            content,
        }
    }
    pub fn to_bytes(&self) -> BytesMut {
        let mut bytes = BytesMut::new();
        bytes.put(&self.length[..]);
        bytes.put_u8(self.kind);
        bytes.put(&self.content[..]);
        bytes
    }

    pub async fn from_stream(stream: &mut TcpStream) -> PeerMessage {
        let mut msg_buf = PeerMessage::new();
        stream
            .read_exact(&mut msg_buf.length)
            .await
            .expect("Read message length");
        msg_buf.content.resize(
            (u32::from_be_bytes(msg_buf.length) - 1) as usize,
            Default::default(),
        );
        msg_buf.kind = stream.read_u8().await.expect("Read kind byte");
        stream
            .read_exact(&mut msg_buf.content)
            .await
            .expect("Read peer message contents");
        msg_buf
    }

    fn iterate_contents<I>(bytes: I) -> impl Iterator<Item = usize>
    where
        I: IntoIterator<Item = u8>,
    {
        bytes.into_iter().enumerate().flat_map(|(i, val)| {
            (0..u8::BITS as usize)
                .filter_map(move |shift| (val & (128 >> shift) > 0).then_some(i * 8 + shift))
        })
    }
    pub fn piece_indices(&self) -> impl Iterator<Item = usize> + '_ {
        // Sadly this still copies the underlying byte array, but only when the iterator being invoked.
        // And the byte array should only be couple bytes large.
        // The benefit is we can eliminate duplicated code.
        Self::iterate_contents(self.content.iter().copied())
    }

    pub fn into_piece_indices(self) -> impl Iterator<Item = usize> {
        Self::iterate_contents(self.content)
    }
}
impl Handshake {
    pub fn to_bytes(&self) -> BytesMut {
        let mut bytes = BytesMut::new();
        bytes.put_u8(self.length);
        bytes.put(&self.string[..]);
        bytes.put(&self.reserved[..]);
        bytes.put(&self.info_hash[..]);
        bytes.put(&self.peer_id[..]);
        bytes
    }

    pub fn from_bytes(bytes: &[u8; 68]) -> Self {
        Handshake {
            length: bytes[0],
            string: bytes[1..20].try_into().expect("deserialize string"),
            reserved: bytes[20..28].try_into().expect("deserialize reserved"),
            info_hash: bytes[28..48].try_into().expect("deserialize info_hash"),
            peer_id: bytes[48..68].try_into().expect("deserialize peer_id"),
        }
    }
}
pub struct PeerMessageCodec {}

const MAX: usize = 2 * 16 * 1024;

impl Decoder for PeerMessageCodec {
    type Item = PeerMessage;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() <= 4 {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Read length marker.
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;

        // Check that the length is not too large to avoid a denial-of-service
        // attack where the server runs out of memory.
        if length > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            ));
        }

        if src.len() < 4 + length {
            // The full string has not yet arrived.
            //
            // We reserve more space in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(4 + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame.
            return Ok(None);
        }

        // Use advance to modify src such that it no longer contains
        // this frame.
        let mut peer_message = PeerMessage::new();
        peer_message.length = length_bytes;
        peer_message.kind = src[4];
        peer_message.content = src[5..(5 + length - 1)].to_vec();
        src.advance(4 + length);

        Ok(Some(peer_message))
    }
}
impl Encoder<PeerMessage> for PeerMessageCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: PeerMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Don't send a string if it is longer than the other end will
        // accept.
        let length = u32::from_be_bytes(item.length) as usize;
        if length > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            ));
        }

        // Reserve space in the buffer.
        dst.reserve(4 + length);

        // Write the length and string to the buffer.
        dst.extend_from_slice(&item.length);
        dst.put_u8(item.kind);
        dst.extend_from_slice(&item.content);
        Ok(())
    }
}
#[derive(Debug, PartialEq)]
pub enum Status {
    Choked,
    Unchoked,
}
pub struct PeerHandle {
    pub command_tx: tokio::sync::mpsc::Sender<PeerMessage>,
    pub status_rx: tokio::sync::watch::Receiver<Option<Status>>,
    pub bitfield_rx: tokio::sync::watch::Receiver<Option<Bitfield>>,
}
impl PeerHandle {
    pub fn is_unchocked(&self) -> bool {
        let status = match self.status_rx.borrow().as_ref() {
            Some(&Status::Choked) => "peer chocked",
            Some(&Status::Unchoked) => "peer unchocked",
            None => "peer not send chocked/unchocked"
        };
        eprintln!("{status}");

        self.status_rx.borrow().as_ref() == Some(&Status::Unchoked)
    }
    pub fn has_piece(&self, i: usize) -> anyhow::Result<bool> {
        match self.bitfield_rx.borrow().as_ref() {
            Some(bitfield) => Ok(bitfield.has_piece(i)),
            None => anyhow::bail!("No bitfield from peer")
        }
    }
}

#[derive(Debug)]
pub struct Bitfield {
    bytes: Vec<u8>,
}
impl Bitfield {
    pub fn new(bytes: Vec<u8>) -> Self {
        Bitfield {
            bytes
        }
    }
    pub fn has_piece(&self, i: usize) -> bool {
        let byte_i = i / (u8::BITS as usize);
        let bit_i = i % (u8::BITS as usize);

        let byte = self.bytes[byte_i];
        let right_shift = u8::BITS - (bit_i as u32) - 1;
        eprintln!("check piece {i} in byte {byte:b}");
        (byte >> right_shift) & 1 != 0
    }
}
pub enum PeerAgentCommand {
    Handshake {
        handshake: Handshake,
        tx: tokio::sync::oneshot::Sender<anyhow::Result<[u8; 20]>>,
    },
    Request {
        message: PeerMessage,
        tx: tokio::sync::oneshot::Sender<anyhow::Result<(usize, usize, Vec<u8>)>>,
    },
}
