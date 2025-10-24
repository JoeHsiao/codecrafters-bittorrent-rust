use crate::peer::{Handshake, PeerMessage, PeerMessageCodec};
use crate::torrent::{FileList, Torrent};
use crate::utils::{get_bitfield, get_peers};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::cmp::min;
use std::io::SeekFrom;
use std::net::SocketAddrV4;
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

#[derive(Debug)]
pub struct Downloader {
    torrent: Torrent,
    pieces_state: Vec<bool>,
    who_has_pieces: Vec<Vec<SocketAddrV4>>,
}

const PIECE_SHA1_LENGTH: usize = 20;

impl Downloader {
    /// We assume that peers have all the pieces.
    /// todo!("Handle the situation where pieces are missing from peers")
    pub async fn download_all(&mut self) {
        for i in 0..self.pieces_state.len() {
            if self.pieces_state[i] {
                continue;
            }
            let peers = &self.who_has_pieces[i];
            if peers.is_empty() {
                eprintln!("Nobody has piece {}", i);
                todo!("handle the case if nobody has this piece")
            }
            let piece = Self::download_piece_in_memory(&self.torrent, &peers[0], i).await;
            if let Err(err) = self.write_piece_to_files(i, piece).await {
                panic!("{}: Failed to write piece {} to file", err, i);
            } else {
                self.pieces_state[i] = true;
            }
        }
    }
    pub async fn download_piece_in_memory(
        torrent: &Torrent,
        peer: &SocketAddrV4,
        piece_index: usize,
    ) -> Vec<u8> {
        // Perform handshake
        let mut stream = TcpStream::connect(peer).await.expect("Tcp connection");
        assert!(
            piece_index < torrent.info.pieces.len().div_ceil(20),
            "piece index '{piece_index}' is over the boundary"
        );

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
            min(
                length - (piece_index * torrent.info.piece_length),
                torrent.info.piece_length,
            )
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
                    u32::from_be_bytes(block_offset.try_into().expect("Parse block offset"))
                        as usize;

                data_buf[block_offset..block_offset + data.len()].copy_from_slice(data);
            }
        }
        let piece_sha1_offset = piece_index * PIECE_SHA1_LENGTH;
        let expected_hash = &torrent.info.pieces[piece_sha1_offset..][..PIECE_SHA1_LENGTH];

        assert_eq!(
            *expected_hash,
            *Sha1::digest(&data_buf),
            "Incorrect hash: piece {piece_index}. piece_length: {piece_length}"
        );
        data_buf
    }
    pub async fn new(torrent_path: impl AsRef<Path>) -> Result<Self> {
        let t = Torrent::from_file(torrent_path);
        let pieces_state = match Self::load_pieces_state() {
            Some(state) => state,
            None => {
                vec![false; t.info.pieces.len() / PIECE_SHA1_LENGTH]
            }
        };
        let peers = get_peers(&t).await;
        let mut who_has_pieces = vec![Vec::new(); pieces_state.len()];
        Self::fill_who_has_pieces(&peers, &mut who_has_pieces, &t.info_sha1()).await;

        Ok(Downloader {
            torrent: t,
            pieces_state,
            who_has_pieces,
        })
    }

    fn get_where_to_write(&self, piece_i: usize, piece_len: usize) -> Vec<(&Path, usize, usize)> {
        match &self.torrent.info.files {
            FileList::SingleFile { length } => {
                let global_start = self.torrent.info.piece_length * piece_i;
                assert!(
                    global_start + piece_len <= *length,
                    "piece will be written over file boundary"
                );
                vec![(self.torrent.info.name.as_ref(), global_start, piece_len)]
            }
            FileList::MultiFile { .. } => {
                todo!()
            }
        }
    }
    async fn write_piece_to_files(&self, piece_i: usize, piece: Vec<u8>) -> Result<()> {
        let dest = self.get_where_to_write(piece_i, piece.len());
        eprintln!("{:?}", dest);

        let mut piece = &piece[..];
        for (path, start, size) in dest {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .open(path)
                .await
                .context("Open file")?;
            file.seek(SeekFrom::Start(start as u64))
                .await
                .context("Seek file offset")?;
            let (val, rest) = piece.split_at(size);
            file.write_all(val).await.context("Write to file")?;
            piece = rest;
        }
        Ok(())
    }

    fn load_pieces_state() -> Option<Vec<bool>> {
        None
    }
    async fn fill_who_has_pieces(
        peers: &Vec<SocketAddrV4>,
        who_has_pieces: &mut Vec<Vec<SocketAddrV4>>,
        info_hash: &[u8; 20],
    ) {
        for peer in peers {
            let bitfield = get_bitfield(peer, info_hash).await;
            bitfield.for_each(|x| who_has_pieces[x].push(*peer));
        }
    }
}
