use std::cmp::min;
use std::fs;
use crate::peer::{Handshake, PeerMessage, PeerMessageCodec};
use crate::torrent::{FileList, Torrent};
use crate::tracker::TrackerResponse;
use futures::{SinkExt, StreamExt};
use reqwest::{Client, Url};
use std::path::Path;
use std::str::FromStr;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

pub async fn download_piece(
    torrent_path: impl AsRef<Path>,
    save_to: impl AsRef<Path>,
    piece_index: usize,
) {
    let peers = peers(&torrent_path).await;
    if peers.is_empty() {
        panic!("No peers found");
    }

    let peer = &peers[1];

    // Perform handshake
    let mut stream = TcpStream::connect(peer).await.expect("Tcp connection");
    let torrent = Torrent::from_file(torrent_path);

    assert!(piece_index < torrent.info.pieces.len().div_ceil(20), "piece index '{piece_index}' is over the boundary");

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
    let _ = Handshake::from_bytes(&buf);

    // Read a bitfied message
    let mut framed = Framed::new(stream, PeerMessageCodec {});
    if let Some(msg) = framed.next().await {
        let msg = msg.expect("Read bitfield message");
        assert_eq!(msg.kind, 5);
    }

    // Send an interested message
    let interested_msg = PeerMessage {
        length: u32::to_be_bytes(1),
        kind: 2,
        content: vec![],
    };
    framed
        .send(interested_msg)
        .await
        .expect("Send the interested message");

    // Read an unchoke message
    if let Some(msg) = framed.next().await {
        let msg = msg.expect("Read the unchoke message");
        assert_eq!(msg.kind, 1);
    }

    // Request blocks of the desired piece
    const BLOCK_SIZE: usize = 16 * 1024;
    let piece_length = if let FileList::SingleFile { length } = torrent.info.files {
        min(length - (piece_index * torrent.info.piece_length), torrent.info.piece_length)
    } else {
        todo!()
    };

    let n_block = piece_length.div_ceil(BLOCK_SIZE);
    for i in 0..n_block {
        let block_size = min(piece_length - (i * BLOCK_SIZE), BLOCK_SIZE);

        let mut message = PeerMessage::new();
        message.kind = 6;
        message
            .content
            .extend_from_slice(&(piece_index as u32).to_be_bytes());
        message
            .content
            .extend_from_slice(&((i * BLOCK_SIZE) as u32).to_be_bytes());
        message
            .content
            .extend_from_slice(&(block_size as u32).to_be_bytes());

        message.length = ((message.content.len() + 1) as u32).to_be_bytes();
        framed.send(message).await.expect("Send request message");
    }

    let mut data_buf = vec![0; piece_length];
    for _ in 0..n_block {
        if let Some(msg) = framed.next().await {
            let msg = msg.expect("piece message");
            assert_eq!(msg.kind, 7);

            let (index, rest) = msg.content.split_at(4);
            let index = u32::from_be_bytes(index.try_into().expect("Parse piece index"));
            assert_eq!(index as usize, piece_index);

            let (block_offset, data) = rest.split_at(4);
            let block_offset =
                u32::from_be_bytes(block_offset.try_into().expect("Parse block offset")) as usize;

            data_buf[block_offset..block_offset + data.len()].copy_from_slice(data);
        }
    }
    let piece_offset = piece_index * 20;
    let expected_hash = &torrent.info.pieces[piece_offset..piece_offset+20];


    assert_eq!(*expected_hash, *Sha1::digest(&data_buf));
    fs::write(save_to, data_buf).expect("Save piece to disk");
}
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
pub async fn peers(torrent_path: impl AsRef<Path>) -> Vec<String> {
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
