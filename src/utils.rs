use crate::peer::{Handshake, PeerAgentCommand, PeerHandle, PeerMessageCodec};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::io::SeekFrom;
use std::iter;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
// pub async fn get_peers_from_path(torrent_path: impl AsRef<Path>) -> Vec<SocketAddrV4> {
//     let torrent = Torrent::from_file(torrent_path);
//     eprintln!("{:?}", torrent.info.piece_length);
//     get_peers(&torrent).await
// }



// pub async fn handshake(peer_handle: &PeerHandle, handshake: Handshake) -> anyhow::Result<[u8; 20]> {
//     let (tx, rx) = tokio::sync::oneshot::channel();
//     let command = PeerAgentCommand::Handshake {
//         handshake,
//         tx
//     };
//     peer_handle.command_tx.send(command).await.context("Send peer handshake")?;
//     rx.await.context("Handshake failed")?
// }

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

pub fn split_piece_for_files(
    files: Vec<(PathBuf, usize, usize)>,
    piece: Vec<u8>,
) -> impl Iterator<Item = (PathBuf, usize, Vec<u8>)> {
    let mut head = 0;
    let mut files = files.into_iter();

    iter::from_fn(move || match files.next() {
        Some((file, offset, size)) => {
            assert!(head + size <= piece.len(), "piece length not long enough to split into files");
            let result = Some((file, offset, piece[head..size].to_vec()));
            head += size;
            result
        }
        None => None,
    })
}

pub async fn write_to_file(path: &Path, offset: usize, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(path)
        .await
        .context("Open file")?;

    file.seek(SeekFrom::Start(offset as u64))
        .await
        .context("Seek file offset")?;

    file.write_all(&data).await.context("Write to file")?;
    Ok(())
}

