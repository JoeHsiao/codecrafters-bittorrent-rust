use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};

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