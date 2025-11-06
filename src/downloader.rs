use crate::consts::BLOCK_SIZE;
use crate::peer::{PeerHandle, PeerMessage};
use crate::peer_agent::PeerAgent;
use crate::piece::Piece;
use crate::torrent::{FileList, Torrent};
use crate::tracker::TrackerResponse;
use crate::utils::write_to_file;
use anyhow::{Context, Result};
use reqwest::{Client, Url};
use sha1::{Digest, Sha1};
use std::cmp::min;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::str::FromStr;

pub struct Downloader {
    id: [u8; 20],
    torrent: Torrent,
    pieces_state: Vec<bool>,
    peer_handles: Vec<PeerHandle>,
    block_tx: tokio::sync::mpsc::Sender<(usize, usize, Vec<u8>)>,
    block_rx: tokio::sync::mpsc::Receiver<(usize, usize, Vec<u8>)>,
}

const PIECE_SHA1_LENGTH: usize = 20;

impl Downloader {
    /// We assume that peers have all the pieces.
    /// todo!("Handle the situation where pieces are missing from peers")
    pub async fn new(torrent_path: impl AsRef<Path>) -> Result<Self> {
        let t = Torrent::from_file(torrent_path);
        let pieces_state = match Self::load_pieces_state() {
            Some(state) => state,
            None => {
                vec![false; t.info.pieces.len() / PIECE_SHA1_LENGTH]
            }
        };
        let (block_tx, block_rx) = tokio::sync::mpsc::channel(32);

        Ok(Downloader {
            id: "cc112233445566778899"
                .as_bytes()
                .try_into()
                .expect("Generate my id"),
            torrent: t,
            pieces_state,
            peer_handles: Vec::new(),
            block_tx,
            block_rx,
        })
    }

    pub async fn spawn_agent(&self, ip: SocketAddrV4) -> anyhow::Result<PeerHandle> {
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(32);
        let (status_tx, status_rx) = tokio::sync::watch::channel(None);
        let (bitfield_tx, bitfield_rx) = tokio::sync::watch::channel(None);

        let id = self.id.clone();
        let info_hash = self.torrent.info_sha1().clone();
        let block_tx = self.block_tx.clone();

        eprintln!("Spawning agent {ip}");

        match PeerAgent::new(
            ip,
            id,
            info_hash,
            command_rx,
            block_tx,
            status_tx,
            bitfield_tx,
        )
        .await
        {
            Ok(peer_agent) => {
                let handle = PeerHandle {
                    command_tx,
                    status_rx,
                    bitfield_rx,
                };
                tokio::spawn(async move { peer_agent.run().await });
                Ok(handle)
            }
            Err(err) => {
                anyhow::bail!("Failed to spawn agent {ip}: {err}");
            }
        }
    }

    pub async fn connect_to_peers(&mut self, n: usize) -> Result<()> {
        let peer_ips = self
            .get_peers()
            .await
            .context("Get peer list from tracker")?;

        let mut peer_handles = Vec::new();
        for ip in peer_ips {
            let handle = self.spawn_agent(ip).await.context("Connect to peer")?;
            peer_handles.push(handle);

            if peer_handles.len() == n {
                break;
            }
        }

        for handle in peer_handles.iter_mut() {
            let bitfield = &mut handle.bitfield_rx;
            while bitfield.borrow().is_none() {
                eprintln!("Wait for bitfield message");
                if bitfield.changed().await.is_err() {
                    break;
                }
            }
            let interested_msg = PeerMessage {
                length: u32::to_be_bytes(1),
                kind: 2,
                content: vec![],
            };
            handle
                .command_tx
                .send(interested_msg)
                .await
                .context("Send interested")?;

            let status = &mut handle.status_rx;
            while status.borrow().is_none() {
                eprintln!("Wait for choked/unchoked message");
                if status.changed().await.is_err() {
                    break;
                }
            }
        }

        self.peer_handles = peer_handles;
        Ok(())
    }
    pub async fn download_range<I>(
        &mut self,
        pieces_range: I,
    ) -> anyhow::Result<HashMap<usize, Vec<u8>>>
    where
        I: IntoIterator<Item = usize> + Clone,
    {
        let pieces_range_clone = pieces_range.clone();
        for i in pieces_range_clone {
            self.request_piece(i)
                .await
                .context("Request piece from peers")?;
        }

        self.retrieve_pieces(pieces_range)
            .await
            .context("Retrieve requested pieces")
    }
    pub async fn download_all(&mut self) -> anyhow::Result<()> {
        let n_pieces = self.pieces_state.len();
        // Buffer this many pieces in memory before writing them to disk.
        // A piece is commonly 256k bytes.
        let buf_n = 2 /* user config */;
        for start in (0..n_pieces).step_by(buf_n) {
            let range = start..(start + buf_n).min(n_pieces);
            let pieces_bytes = self.download_range(range).await.context("Download range")?;
            for (i, bytes) in pieces_bytes {
                self.write_piece(i, &bytes)
                    .await
                    .context("Write piece to file")?;
                self.pieces_state[i] = true;
            }
        }
        Ok(())
    }
    fn expected_hash(&self, piece_i: usize) -> anyhow::Result<[u8; 20]> {
        let piece_sha1_offset = piece_i * PIECE_SHA1_LENGTH;
        let expected_hash: [u8; 20] = self.torrent.info.pieces[piece_sha1_offset..]
            [..PIECE_SHA1_LENGTH]
            .try_into()
            .context("get piece hash")?;
        Ok(expected_hash)
    }
    async fn retrieve_pieces<I>(&mut self, indices: I) -> anyhow::Result<HashMap<usize, Vec<u8>>>
    where
        I: IntoIterator<Item = usize>,
    {
        let mut pieces_buf = HashMap::new();
        let mut result = HashMap::new();

        for i in indices {
            let piece_length = self.piece_length(i);
            pieces_buf.insert(i, Piece::new(piece_length));
        }

        if pieces_buf.is_empty() {
            return Ok(result);
        }

        while let Some((piece_i, offset, block)) = self.block_rx.recv().await {
            let Some(piece) = pieces_buf.get_mut(&piece_i) else {
                anyhow::bail!("Receive a block that we did not request");
            };
            piece
                .write(offset, block.as_slice())
                .context("Save block in memory")?;
            if piece.done() {
                let bytes = pieces_buf
                    .remove(&piece_i)
                    .context("Remove piece from buf")?
                    .bytes();
                anyhow::ensure!(
                    self.expected_hash(piece_i)
                        .context("get expected piece hash")?
                        == *Sha1::digest(&bytes)
                );
                result.insert(piece_i, bytes);
            }
            if pieces_buf.is_empty() {
                break;
            }
        }

        Ok(result)
    }
    fn total_length(&self) -> usize {
        match &self.torrent.info.files {
            FileList::SingleFile { length } => *length,
            FileList::MultiFile { files } => files.iter().map(|f| f.length).sum(),
        }
    }
    fn piece_length(&self, i: usize) -> usize {
        min(
            self.total_length() - (i * self.torrent.info.piece_length),
            self.torrent.info.piece_length,
        )
    }

    async fn request_piece(&self, piece_i: usize) -> anyhow::Result<()> {
        let peers = &self.peer_handles;
        anyhow::ensure!(!peers.is_empty(), "No peers available");
        let piece_length = self.piece_length(piece_i);
        let n_block = piece_length.div_ceil(BLOCK_SIZE);
        let n_peers = peers.len();

        // round-robin
        for i in 0..n_block {
            // TODO skip a handle if it is either choked or not having the desired piece
            let handle = &peers[i % n_peers];
            let offset = i * BLOCK_SIZE;
            let block_size = min(piece_length - offset, BLOCK_SIZE);
            let req = PeerMessage::new_request(piece_i, offset, block_size);
            handle
                .command_tx
                .send(req)
                .await
                .context("Send Request message")?;
        }
        Ok(())
    }

    async fn write_piece(&self, i: usize, piece: &Vec<u8>) -> anyhow::Result<()> {
        let files = match &self.torrent.info.files {
            FileList::SingleFile { length } => {
                let global_offset = self.torrent.info.piece_length * i;
                assert!(
                    global_offset + piece.len() <= *length,
                    "piece too large to insert into the file"
                );
                vec![(&self.torrent.info.name, global_offset, piece.as_slice())]
            }
            FileList::MultiFile { .. } => {
                todo!()
            }
        };

        for (path, offset, data) in files {
            write_to_file(path, offset, data)
                .await
                .with_context(|| format!("write piece to {}", path.display()))?;
        }

        Ok(())
    }

    fn load_pieces_state() -> Option<Vec<bool>> {
        None
    }

    pub async fn get_peers(&self) -> anyhow::Result<Vec<SocketAddrV4>> {
        let mut url = Url::from_str(&self.torrent.announce).context("Get tracker announce")?;

        let query = format!(
            "port=6881&uploaded=0&downloaded=0&compact=1&peer_id={}&left={}&info_hash={}",
            String::from_utf8(self.id.to_vec()).context("Convert my id to String")?,
            self.total_length(),
            urlencoding::encode_binary(&self.torrent.info_sha1())
        );
        url.set_query(Some(&query));

        let client = Client::new();
        let res = client
            .get(url)
            .send()
            .await
            .context("Send tracker query")?
            .bytes()
            .await
            .context("Get tracker response")?;

        let tracker_res: TrackerResponse =
            serde_bencode::from_bytes(&res).context("Deserialize tracker response")?;

        let mut peers = Vec::new();
        for bytes in tracker_res.peers.chunks_exact(6) {
            let (ip, port) = bytes.split_at(4);
            peers.push(SocketAddrV4::new(
                Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]),
                u16::from_be_bytes([port[0], port[1]]),
            ));
        }
        Ok(peers)
    }
}
