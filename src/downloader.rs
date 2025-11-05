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

    pub async fn spawn_agent(&self, ip: SocketAddrV4) -> anyhow::Result<PeerHandle> {
        let (request_tx, request_rx) = tokio::sync::mpsc::channel(32);
        let (status_tx, status_rx) = tokio::sync::watch::channel(None);
        let (bitfield_tx, bitfield_rx) = tokio::sync::watch::channel(None);

        let id = self.id.clone();
        let info_hash = self.torrent.info_sha1().clone();
        eprintln!("Spawning agent {ip}");

        match PeerAgent::new(ip, id, info_hash, request_tx, status_tx, bitfield_tx).await {
            Ok((mut agent, agent_sender)) => {
                let handle = PeerHandle {
                    request_rx,
                    agent_sender,
                    status_rx,
                    bitfield_rx,
                };
                tokio::spawn(async move { agent.run().await });
                Ok(handle)
            }
            Err(err) => {
                eprintln!("Failed to spawn agent {ip}");
                anyhow::bail!("Failed to spawn agent");
            }
        }
    }

    pub async fn download_all_concurrently(
        &mut self,
        mut peer_handles: Vec<PeerHandle>,
    ) -> anyhow::Result<()> {
        let peer_ips = self
            .get_peers()
            .await
            .context("Get peer list from tracker")?;

        let use_n_peers = 2 /* user config */;

        for ip in peer_ips {
            let handle = self.spawn_agent(ip).await.context("Connect to peer")?;
            peer_handles.push(handle);

            if peer_handles.len() == use_n_peers {
                break;
            }
        }

        for handle in peer_handles.iter_mut() {
            // Wait for choked/unchoked
            let bitfield = &mut handle.bitfield_rx;
            while bitfield.borrow().is_none() {
                eprintln!("waiting for bitfield message");
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
                .agent_sender
                .send(interested_msg)
                .await
                .context("Send interested")?;

            let status = &mut handle.status_rx;
            while status.borrow().is_none() {
                eprintln!("waiting for choked/unchoked message");
                if status.changed().await.is_err() {
                    break;
                }
            }
        }

        eprintln!("Agents are ready!");

        let n_piece = self.pieces_state.len();
        for i in 0..n_piece {
            if self.pieces_state[i] {
                continue;
            }
            // let peers = self
            //     .get_unchocked_peers_for_piece(i)
            //     .context("Get unchocked peers")?;
            let piece_length = self.piece_length(i);
            let piece_sha1_offset = i * PIECE_SHA1_LENGTH;
            let expected_hash: [u8; 20] = self.torrent.info.pieces[piece_sha1_offset..]
                [..PIECE_SHA1_LENGTH]
                .try_into()
                .expect("get piece hash");

            let piece =
                Self::download_piece_in_memory(&mut peer_handles, i, piece_length, expected_hash)
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

    pub async fn download_piece_in_memory(
        peers: &mut Vec<PeerHandle>,
        piece_i: usize,
        piece_length: usize,
        expected_hash: [u8; 20],
    ) -> anyhow::Result<Vec<u8>> {
        // let peers = &mut self.peer_handles;
        // let piece_length = self.piece_length(piece_i);
        // let expected_hash = self.torrent.info_sha1();
        anyhow::ensure!(!peers.is_empty(), "No peers available");
        // Send an interested message
        // let interested_msg = PeerMessage {
        //     length: u32::to_be_bytes(1),
        //     kind: 2,
        //     content: vec![],
        // };
        // framed
        //     .send(interested_msg)
        //     .await
        //     .expect("Send the interested message");
        //
        // // Read an unchoke message
        // if let Some(msg) = framed.next().await {
        //     let msg = msg.expect("Read the unchoke message");
        //     assert_eq!(msg.kind, 1);
        // }

        let n_block = piece_length.div_ceil(BLOCK_SIZE);

        let mut piece_buf = vec![0; piece_length];
        let n_peers = peers.len();
        // round-robin peers
        for i in 0..n_block {
            // TODO skip handles that are either choked or not having this piece
            let handle = &mut peers[i % n_peers];
            let offset = i * BLOCK_SIZE;
            let block_size = min(piece_length - offset, BLOCK_SIZE);
            let req = PeerMessage::new_request(piece_i, offset, block_size);
            handle
                .agent_sender
                .send(req)
                .await
                .context("Send Request message")?;

            let (piece_i, offset, block) = handle.request_rx.recv().await.expect("Reeceive block");
            anyhow::ensure!(block_size == block.len());
            piece_buf[offset..][..block_size].copy_from_slice(block.as_slice());
        }

        // let mut futures = FuturesUnordered::new();
        // for rx in rxs {
        //     futures.push(async move { rx.await });
        // }
        //
        // while let Some(rx) = futures.next().await {
        //     let rx = rx.context("Response from Request command")?;
        //     let (piece_ind, offset, block) = rx.context("Get block data")?;
        //
        //     eprintln!("Got back piece_i: {piece_ind}, offset: {offset}");
        //
        //     anyhow::ensure!(piece_ind == piece_i, "Receive block for another piece");
        //     piece_buf[offset..][..block.len()].copy_from_slice(block.as_slice());
        // }
        // let mut responses = futures_util::stream::iter(rxs.into_iter());
        // while let Some(rs) = responses.next().await {
        //     // (Result<Vec<u8>>, usize)
        //     let (block, offset) = rs.await.context("Response of command Request")?;
        //     let block = block.with_context(|| format!("Cannot get block with offset {offset}"))?;
        //     piece_buf[offset..][..block.len()].copy_from_slice(block.as_slice());
        // }

        // for _ in 0..n_block {
        //     if let Some(msg) = framed.next().await {
        //         let msg = msg.expect("piece message");
        //         assert_eq!(msg.kind, 7);
        //
        //         let (index, rest) = msg.content.split_at(4);
        //         let index = u32::from_be_bytes(index.try_into().expect("Parse piece index"));
        //         assert_eq!(index as usize, piece_index);
        //
        //         let (block_offset, data) = rest.split_at(4);
        //         let block_offset =
        //             u32::from_be_bytes(block_offset.try_into().expect("Parse block offset"))
        //                 as usize;
        //
        //         data_buf[block_offset..block_offset + data.len()].copy_from_slice(data);
        //     }
        // }
        // let piece_sha1_offset = piece_i * PIECE_SHA1_LENGTH;
        // let expected_hash = &self.torrent.info.pieces[piece_sha1_offset..][..PIECE_SHA1_LENGTH];

        anyhow::ensure!(
            expected_hash == *Sha1::digest(&piece_buf),
            "Incorrect hash: piece {piece_i}. piece_length: {piece_length}"
        );
        Ok(piece_buf)
    }

    pub async fn new(torrent_path: impl AsRef<Path>) -> Result<Self> {
        let t = Torrent::from_file(torrent_path);
        let pieces_state = match Self::load_pieces_state() {
            Some(state) => state,
            None => {
                vec![false; t.info.pieces.len() / PIECE_SHA1_LENGTH]
            }
        };

        Ok(Downloader {
            id: "cc112233445566778899"
                .as_bytes()
                .try_into()
                .expect("Generate my id"),
            torrent: t,
            pieces_state,
            peer_handles: Vec::new(),
        })
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
