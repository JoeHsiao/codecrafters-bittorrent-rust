use crate::peer::{Bitfield, Handshake, PeerMessage, PeerMessageCodec, Status};
use anyhow::Context;
use futures_util::sink::SinkExt;
use futures_util::StreamExt;
use std::net::SocketAddrV4;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

pub struct PeerAgent {
    command_rx: tokio::sync::mpsc::Receiver<PeerMessage>,
    block_tx: tokio::sync::mpsc::Sender<(usize, usize, Vec<u8>)>,
    status_tx: tokio::sync::watch::Sender<Option<Status>>,
    bitfield_tx: tokio::sync::watch::Sender<Option<Bitfield>>,

    stream: Framed<TcpStream, PeerMessageCodec>,
}

impl PeerAgent {
    pub async fn new(
        ip: SocketAddrV4,
        my_id: [u8; 20],
        info_hash: [u8; 20],
        command_rx: tokio::sync::mpsc::Receiver<PeerMessage>,
        block_tx: tokio::sync::mpsc::Sender<(usize, usize, Vec<u8>)>,
        status_tx: tokio::sync::watch::Sender<Option<Status>>,
        bitfield_tx: tokio::sync::watch::Sender<Option<Bitfield>>,
    ) -> anyhow::Result<Self> {
        let mut stream = TcpStream::connect(ip).await.expect("Connect to peer");

        let handshake = Handshake {
            length: 19,
            string: b"BitTorrent protocol".to_owned(),
            reserved: [0; 8],
            peer_id: my_id,
            info_hash,
        };

        stream
            .write_all(handshake.to_bytes().as_ref())
            .await
            .context("Write to Tcp")?;

        let mut buf = [0; 68];
        stream.read_exact(&mut buf).await.expect("Read from Tcp");
        let response: Handshake = Handshake::from_bytes(&buf);
        anyhow::ensure!(response.info_hash == info_hash, "info_hash does not match");

        eprintln!("[Agent] Handshake done");

        let stream = Framed::new(stream, PeerMessageCodec {});
        Ok(Self {
            command_rx,
            block_tx,
            status_tx,
            bitfield_tx,
            stream,
        })
    }

    pub async fn run(self) {
        let mut command_rx = self.command_rx;
        let block_tx = self.block_tx;
        let mut stream = self.stream;
        let status_tx = self.status_tx;
        let bitfield_tx = self.bitfield_tx;

        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(peer_message)) => {
                            match peer_message.kind {
                                0 => {
                                    status_tx.send(Some(Status::Choked)).expect("[Agent] Update status to choked");
                                }
                                1 => {
                                    status_tx.send(Some(Status::Unchoked)).expect("[Agent] Update status to unchoked");
                                }
                                5 => {
                                    eprintln!("[Agent] received bitfield");
                                    bitfield_tx
                                        .send(Some(Bitfield::new(peer_message.content)))
                                        .unwrap();
                                }
                                7 => {
                                    let (piece_i, rest) = peer_message.content.split_at(4);
                                    let (offset, data) = rest.split_at(4);

                                    let piece_i =
                                        u32::from_be_bytes(piece_i.try_into().expect("Extract piece index"))
                                        as usize;
                                    let offset =
                                        u32::from_be_bytes(offset.try_into().expect("Extract block offset"))
                                        as usize;

                                    eprintln!("[Agent] Got piece: {piece_i}, block: {offset}");

                                    block_tx
                                        .send((piece_i, offset, data.to_vec()))
                                        .await
                                        .expect("Send block data back");
                                }
                                a => {
                                    eprintln!("[Agent] Unknown message type: {a}");
                                }
                            }
                        }
                        Some(Err(err)) => {
                            eprintln!("[Agent] Received a message of unknown format: {err}");
                        }
                        None => {
                            eprintln!("[Agent] Peer closed the connection");
                            break;
                        }
                    }
                },
                command = command_rx.recv() => {
                    match command {
                        Some(peer_message) => {
                            stream.send(peer_message).await.expect("[Agent] Sending request to peer");
                        }
                        None => {
                            eprintln!("[Agent] Existing...");
                            break; // graceful shutdown
                        }
                    }

                }
            }
        }
    }
}
