use crate::peer::{Handshake, PeerMessage, PeerMessageCodec};
use crate::torrent::{FileList, Torrent};
use crate::utils::{get_bitfield, get_peers, split_piece_by_file, write_to_file};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::cmp::min;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

#[derive(Debug)]
pub struct Downloader {
    torrent: Arc<Torrent>,
    pieces_state: Vec<bool>,
    who_has_pieces: Vec<Vec<SocketAddrV4>>,
}

const PIECE_SHA1_LENGTH: usize = 20;

impl Downloader {
    /// We assume that peers have all the pieces.
    /// todo!("Handle the situation where pieces are missing from peers")
    pub async fn download_all_sequential(&mut self) {
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
            match self.write_piece_to_files(i, piece).await {
                Ok(()) => self.pieces_state[i] = true,
                Err(err) => panic!("{}: Failed to write piece {} to file", err, i),
            }
        }
    }
    pub async fn download_all_concurrent(&mut self) {
        let mut read_handles = Vec::new();
        let mut write_handles = Vec::new();

        for i in 0..self.pieces_state.len() {
            if self.pieces_state[i] {
                continue;
            }

            let peers = &self.who_has_pieces[i];
            if peers.is_empty() {
                eprintln!("Nobody has piece {}", i);
                todo!("handle the case if nobody has this piece")
            }

            let peer = self.who_has_pieces[i][0].clone();
            let torrent = Arc::clone(&self.torrent);

            read_handles.push(tokio::spawn(async move {
                (i, Self::download_piece_in_memory(&torrent, &peer, i).await)
            }));
        }
        for read_handle in read_handles {
            let (i, piece) = read_handle.await.expect("Join read handle");
            let files = self.get_where_to_write(i, piece.len()).clone();

            let mut split_into = split_piece_by_file(files, piece);
            while let Some((path, offset, data)) = split_into.next() {
                write_handles.push(tokio::spawn(async move {
                    (i, write_to_file(&path, offset, data).await)
                }));
            }
        }

        for write_handle in write_handles {
            let (i, res) = write_handle.await.expect("Join write handle");
            if let Ok(_) = res {
                self.pieces_state[i] = true;
            }
        }

        println!("done!");
    }

    async fn write_piece_to_files(&self, piece_i: usize, piece: Vec<u8>) -> Result<()> {
        let dests = self.get_where_to_write(piece_i, piece.len());

        let mut piece = &piece[..];
        for (path, start, size) in dests {
            let (val, rest) = piece.split_at(size);
            write_to_file(&path, start, val.to_vec())
                .await
                .context("Write piece to file")?;
            piece = rest;
        }
        Ok(())
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
            // We use a different peer id each time a piece is downloaded because this function is
            // called concurrently by multiple threads. Using the same peer id when talking to the same
            // peer on different threads would mess up the communications.
            // In reality, we should establish a connection from a peer only once and re-use it for
            // subsequent downloads if they are from the same peer.
            peer_id: Sha1::digest(format!("BT piece {piece_index}"))
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
            torrent: Arc::new(t),
            pieces_state,
            who_has_pieces,
        })
    }

    // todo!("Rewrite this function to return an iterator instead")
    fn get_where_to_write(&self, piece_i: usize, piece_len: usize) -> Vec<(PathBuf, usize, usize)> {
        match &self.torrent.info.files {
            FileList::SingleFile { length } => {
                let global_start = self.torrent.info.piece_length * piece_i;
                assert!(
                    global_start + piece_len <= *length,
                    "piece will be written over file boundary"
                );
                vec![(
                    self.torrent.info.name.clone().into(),
                    global_start,
                    piece_len,
                )]
            }
            FileList::MultiFile { .. } => {
                todo!()
            }
        }
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
