use crate::peer::{Handshake, PeerMessageCodec};
use crate::torrent::Torrent;
use crate::tracker::TrackerResponse;
use futures::StreamExt;
use reqwest::{Client, Url};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

pub async fn get_peers(torrent: &Torrent) -> Vec<SocketAddrV4> {
    let mut url = Url::from_str(&torrent.announce).expect("Tracker announce");
    let query = format!(
        "port=6881&uploaded=0&downloaded=0&compact=1&peer_id={}&left={}&info_hash={}",
        "12345678900987654321",
        torrent.info.piece_length,
        urlencoding::encode_binary(&torrent.info_sha1())
    );
    url.set_query(Some(&query));

    let client = Client::new();
    let res = client
        .get(url)
        .send()
        .await
        .expect("Response from peers")
        .bytes()
        .await
        .expect("Get response bytes");

    eprintln!("res bytes: {:?}", res);
    let tracker_res: TrackerResponse =
        serde_bencode::from_bytes(&res).expect("Deserialize tracker response");

    let mut peers = Vec::new();
    for bytes in tracker_res.peers.chunks_exact(6) {
        let (ip, port) = bytes.split_at(4);
        peers.push(SocketAddrV4::new(
            Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]),
            u16::from_be_bytes([port[0], port[1]]),
        ));
    }
    peers
}

pub async fn get_peers_from_path(torrent_path: impl AsRef<Path>) -> Vec<SocketAddrV4> {
    let torrent = Torrent::from_file(torrent_path);
    eprintln!("{:?}", torrent.info.piece_length);
    get_peers(&torrent).await
}

pub async fn get_bitfield(ip: &SocketAddrV4, info_hash: &[u8; 20]) -> impl Iterator<Item = usize> {
    let mut stream = TcpStream::connect(ip).await.expect("Tcp connection");

    let handshake = Handshake {
        length: 19,
        string: b"BitTorrent protocol".to_owned(),
        reserved: [0; 8],
        peer_id: "12345678900987654321"
            .as_bytes()
            .try_into()
            .expect("init peer_id"),
        info_hash: *info_hash,
    };

    stream
        .write_all(handshake.to_bytes().as_ref())
        .await
        .expect("Write handshake to Tcp");

    let mut buf = [0; 68];
    stream.read_exact(&mut buf).await.expect("Read from Tcp");

    let _ = Handshake::from_bytes(&buf);

    let mut framed = Framed::new(stream, PeerMessageCodec {});
    let msg = framed
        .next()
        .await
        .expect("Read message message")
        .expect("Read a bitfield message");

    assert_eq!(msg.kind, 5, "Not a bitfield message, but {}", msg.kind);
    msg.into_piece_indices()
}
