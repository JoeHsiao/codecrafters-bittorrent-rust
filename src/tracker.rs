use serde::{Deserialize};
use serde_bytes::ByteBuf;

#[derive(Debug, Deserialize)]
pub struct TrackerResponse {
    /// List of peers that your client can connect to.
    ///
    /// Each peer is represented using 6 bytes. The first 4 bytes are the peer's IP address and
    /// the last 2 bytes are the peer's port number.
    pub peers: ByteBuf,

    /// The number of seconds the downloader should wait between regular rerequests
    pub interval: usize,
}