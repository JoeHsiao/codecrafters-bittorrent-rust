use crate::peer::Handshake;
use crate::torrent::Torrent;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn handshake(torrent_path: impl AsRef<Path>, ip: &str) -> String {
    let mut stream = TcpStream::connect(ip).await.expect("Tcp connection");
    let torrent = Torrent::from_file(torrent_path);

    let handshake = Handshake {
        length: 19,
        string: b"BitTorrent protocol".to_owned(),
        reserved: [0; 8],
        peer_id: "12345678900987654321"
            .as_bytes()
            .try_into()
            .expect("init peer_id"),
        info_hash: torrent.info_sha1(),
    };

    stream
        .write_all(handshake.to_bytes().as_ref())
        .await
        .expect("Write to Tcp");

    let mut buf = [0; 68];
    stream.read_exact(&mut buf).await.expect("Read from Tcp");

    let response: Handshake = Handshake::from_bytes(&buf);
    hex::encode(response.peer_id)
}
