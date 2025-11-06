use crate::consts::BLOCK_SIZE;
use crate::peer::{PeerAgentCommand, PeerHandle, PeerMessage};
use crate::peer_agent::PeerAgent;
use crate::torrent::{FileList, Torrent};
use crate::tracker::TrackerResponse;
use crate::utils::write_to_file;
use anyhow::{Context, Result};
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url};
use sha1::{Digest, Sha1};
use std::cmp::min;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;

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
    // pub async fn download_all_sequential(&mut self) {
    //     for i in 0..self.pieces_state.len() {
    //         if self.pieces_state[i] {
    //             continue;
    //         }
    //         let peers = &self.who_has_pieces[i];
    //         if peers.is_empty() {
    //             eprintln!("Nobody has piece {}", i);
    //             todo!("handle the case if nobody has this piece")
    //         }
    //         let piece = Self::download_piece_in_memory(&self.torrent, &peers[0], i).await;
    //         match self.write_piece_to_files(i, piece).await {
    //             Ok(()) => self.pieces_state[i] = true,
    //             Err(err) => panic!("{}: Failed to write piece {} to file", err, i),
    //         }
    //     }
    // }
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

    pub async fn spawn_agent(
        &self,
        ip: SocketAddrV4,
    ) -> anyhow::Result<PeerHandle> {
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

    pub async fn spawn_agents(&self, n: usize) -> Result<Vec<PeerHandle>> {
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
            // Wait for choked/unchoked
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

        Ok(peer_handles)
    }
    pub async fn download_all_concurrently(
        &mut self,
        mut peer_handles: Vec<PeerHandle>,
    ) -> anyhow::Result<()> {
        let mut peer_handles = self.spawn_agents(3).await.context("Spawn agents")?;

        eprintln!("Agents are ready!");

        let n_piece = self.pieces_state.len();
        for i in 0..n_piece {
            if self.pieces_state[i] {
                continue;
            }
            let piece_length = self.piece_length(i);
            let piece_sha1_offset = i * PIECE_SHA1_LENGTH;
            let expected_hash: [u8; 20] = self.torrent.info.pieces[piece_sha1_offset..]
                [..PIECE_SHA1_LENGTH]
                .try_into()
                .expect("get piece hash");

            let piece =
                Self::download_piece_in_memory(&mut peer_handles, i, piece_length, expected_hash, &mut self.block_rx)
                    .await
                    .context("Download piece")?;

            self.write_piece(i, piece).await.context("Write piece")?;
            self.pieces_state[i] = true;
        }
        Ok(())
    }

    // async fn write_piece_to_files(&self, piece_i: usize, piece: Vec<u8>) -> Result<()> {
    //     let dests = self.get_where_to_write(piece_i, piece.len());
    //
    //     let mut piece = &piece[..];
    //     for (path, start, size) in dests {
    //         let (val, rest) = piece.split_at(size);
    //         write_to_file(&path, start, val.to_vec())
    //             .await
    //             .context("Write piece to file")?;
    //         piece = rest;
    //     }
    //     Ok(())
    // }
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

    pub async fn request_piece(
        peers: &Vec<PeerHandle>,
        piece_i: usize,
        piece_length: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!peers.is_empty(), "No peers available");
        let n_block = piece_length.div_ceil(BLOCK_SIZE);

        let mut piece_buf = vec![0; piece_length];
        let n_peers = peers.len();
        // round-robin peers
        for i in 0..n_block {
            // TODO skip handles that are either choked or not having this piece
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
    pub async fn download_piece_in_memory(
        peers: &Vec<PeerHandle>,
        piece_i: usize,
        piece_length: usize,
        expected_hash: [u8; 20],
        block_rx: &mut tokio::sync::mpsc::Receiver<(usize, usize, Vec<u8>)>,
    ) -> anyhow::Result<Vec<u8>> {

        Self::request_piece(peers, piece_i, piece_length).await.context("Send requests for piece")?;

        let n_block = piece_length.div_ceil(BLOCK_SIZE);

        let mut piece_buf = vec![0; piece_length];

        let mut counter = 0;
        while let Some((piece_i, offset, data)) = block_rx.recv().await {
            piece_buf[offset..][..data.len()].copy_from_slice(data.as_slice());
            counter += 1;
            if counter == n_block {
                break;
            }
        }

        anyhow::ensure!(
            expected_hash == *Sha1::digest(&piece_buf),
            "Incorrect hash: piece {piece_i}. piece_length: {piece_length}"
        );
        Ok(piece_buf)
    }



    // todo!("Rewrite this function to return an iterator instead")
    // fn get_where_to_write(&self, piece_i: usize, piece_len: usize) -> Vec<(PathBuf, usize, usize)> {
    //     match &self.torrent.info.files {
    //         FileList::SingleFile { length } => {
    //             let global_start = self.torrent.info.piece_length * piece_i;
    //             assert!(
    //                 global_start + piece_len <= *length,
    //                 "piece will be written over file boundary"
    //             );
    //             vec![(
    //                 self.torrent.info.name.clone().into(),
    //                 global_start,
    //                 piece_len,
    //             )]
    //         }
    //         FileList::MultiFile { .. } => {
    //             todo!()
    //         }
    //     }
    // }
    async fn write_piece(&self, i: usize, piece: Vec<u8>) -> anyhow::Result<()> {
        let files = match &self.torrent.info.files {
            FileList::SingleFile { length } => {
                let global_offset = self.torrent.info.piece_length * i;
                assert!(
                    global_offset + piece.len() <= *length,
                    "piece too large to insert into the file"
                );
                vec![(&self.torrent.info.name, global_offset, &piece[..])]
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
    fn get_unchocked_peers_for_piece(&mut self, i: usize) -> Result<Vec<&mut PeerHandle>> {
        let peers = self
            .peer_handles
            .iter_mut()
            .filter(|h| h.is_unchocked())
            .filter_map(|h| match h.has_piece(i) {
                Ok(true) => Some(Ok(h)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()
            .context("Missing bitfield")?;

        Ok(peers)
    }

    // async fn fill_who_has_pieces(
    //     peers: &Vec<SocketAddrV4>,
    //     who_has_pieces: &mut Vec<Vec<SocketAddrV4>>,
    //     info_hash: &[u8; 20],
    // ) {
    //     for peer in peers {
    //         let bitfield = get_bitfield(peer, info_hash).await;
    //         bitfield.for_each(|x| who_has_pieces[x].push(*peer));
    //     }
    // }

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
