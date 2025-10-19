use crate::peer::Handshake;
use crate::torrent::Torrent;
use crate::tracker::TrackerResponse;
use reqwest::{Client, Url};
use std::str::FromStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn handshake(torrent_path: &str, ip: &str) -> String {
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
    stream
        .read_exact(&mut buf)
        .await
        .expect("Read from Tcp");

    let response: Handshake = Handshake::from_bytes(&buf);

    hex::encode(response.peer_id)
}
pub async fn peers(torrent_path: &str) -> Vec<String> {
    let torrent = Torrent::from_file(torrent_path);

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

    let tracker_res: TrackerResponse =
        serde_bencode::from_bytes(&res).expect("Deserialize tracker response");

    let mut peers = Vec::new();
    for bytes in tracker_res.peers.chunks_exact(6) {
        let (ip, port) = bytes.split_at(4);
        peers.push(format!(
            "{}.{}.{}.{}:{}",
            ip[0],
            ip[1],
            ip[2],
            ip[3],
            u16::from_be_bytes([port[0], port[1]])
        ));
    }
    peers
}
