use crate::peer::{Bitfield, Handshake, PeerAgentCommand, PeerMessage, PeerMessageCodec, Status};
use anyhow::Context;
use futures_util::sink::SinkExt;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{StreamExt, TryStreamExt};
use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::codec::Framed;

pub struct PeerAgent {
    stream_receiver: SplitStream<Framed<TcpStream, PeerMessageCodec>>,
    request_tx: tokio::sync::mpsc::Sender<(usize, usize, Vec<u8>)>,
    status_tx: tokio::sync::watch::Sender<Option<Status>>,
    bitfield_tx: tokio::sync::watch::Sender<Option<Bitfield>>,
}

impl PeerAgent {
    pub async fn new(
        ip: SocketAddrV4,
        my_id: [u8; 20],
        info_hash: [u8; 20],
        request_tx: tokio::sync::mpsc::Sender<(usize, usize, Vec<u8>)>,
        status_tx: tokio::sync::watch::Sender<Option<Status>>,
        bitfield_tx: tokio::sync::watch::Sender<Option<Bitfield>>,
    ) -> anyhow::Result<(
        Self,
        SplitSink<Framed<TcpStream, PeerMessageCodec>, PeerMessage>,
    )> {
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

        // Send interested message
        // let interested_msg = PeerMessage {
        //     length: u32::to_be_bytes(1),
        //     kind: 2,
        //     content: vec![],
        // };
        // stream
        //     .write_all(interested_msg.to_bytes().as_ref())
        //     .await
        //     .expect("Send the interested message");
        eprintln!("Handshake done");

        let stream = Framed::new(stream, PeerMessageCodec {});
        let (sender, receiver) = stream.split();

        Ok((
            PeerAgent {
                stream_receiver: receiver,
                request_tx,
                status_tx,
                bitfield_tx,
            },
            sender,
        ))
    }

    pub async fn run(&mut self) {
        loop {
            if let Some(msg) = self.stream_receiver.next().await {
                let msg = msg.expect("Read peer message");
                match msg.kind {
                    7 => {
                        let (piece_i, rest) = msg.content.split_at(4);
                        let (offset, data) = rest.split_at(4);

                        let piece_i =
                            u32::from_be_bytes(piece_i.try_into().expect("Extract piece index"))
                                as usize;
                        let offset =
                            u32::from_be_bytes(offset.try_into().expect("Extract block offset"))
                                as usize;

                        eprintln!("[Agent] Got piece: {piece_i}, block: {offset}");

                        self.request_tx
                            .send((piece_i, offset, data.to_vec()))
                            .await
                            .expect("Send block data back");
                        // match self.block_senders.remove(&(piece_i, offset)) {
                        //     Some(sender) => {
                        //         sender
                        //             .send(Ok((piece_i, offset, data.to_vec())))
                        //             .expect("Send block back");
                        //     }
                        //     None => {
                        //         eprintln!("Agent receives unexpected block (piece_i: {piece_i}, offset: {offset})");
                        //     }
                        // }
                    }
                    5 => {
                        eprintln!("[Agent] received bitfield");
                        // let interested_msg = PeerMessage {
                        //     length: u32::to_be_bytes(1),
                        //     kind: 2,
                        //     content: vec![],
                        // };
                        // self.stream
                        //     .send(interested_msg)
                        //     .await
                        //     .expect("Send the interested message");

                        self.bitfield_tx
                            .send(Some(Bitfield::new(msg.content)))
                            .unwrap();
                    }
                    0 => {
                        eprintln!("[Agent] received Choked");
                        self.status_tx.send(Some(Status::Choked)).unwrap();
                    }
                    1 => {
                        eprintln!("[Agent] received Unchoked");
                        self.status_tx.send(Some(Status::Unchoked)).unwrap();
                    }
                    a => {
                        eprintln!("[Agent] Unknown message type: {a}");
                    }
                }
            }
        }
    }

    // async fn receive_command(&mut self) {
    //     if let Some(msg) = self.command_rx.recv().await {
    //         match msg {
    //             PeerAgentCommand::Request { message, tx } => {
    //                 let piece_i = u32::from_be_bytes(
    //                     message.content[..4]
    //                         .try_into()
    //                         .expect("Extract piece index"),
    //                 ) as usize;
    //                 let offset = u32::from_be_bytes(
    //                     message.content[4..8].try_into().expect("Extract offset"),
    //                 ) as usize;
    //
    //                 self.block_senders.insert((piece_i, offset), tx);
    //
    //                 self.stream
    //                     .send(message)
    //                     .await
    //                     .expect("Send command to peer");
    //                 eprintln!("[Agent] Asking for piece: {piece_i}, block: {offset}");
    //             }
    //             PeerAgentCommand::Handshake { .. } => {}
    //         }
    //     }
    // }
    // async fn receive_from_peer(&mut self, msg: PeerMessage) {
    //     match msg.kind {
    //         7 => {
    //             let (piece_i, rest) = msg.content.split_at(4);
    //             let (offset, data) = rest.split_at(4);
    //
    //             let piece_i =
    //                 u32::from_be_bytes(piece_i.try_into().expect("Extract piece index")) as usize;
    //             let offset =
    //                 u32::from_be_bytes(offset.try_into().expect("Extract block offset")) as usize;
    //
    //             eprintln!("[Agent] Got piece: {piece_i}, block: {offset}");
    //
    //             match self.block_senders.remove(&(piece_i, offset)) {
    //                 Some(sender) => {
    //                     sender
    //                         .send(Ok((piece_i, offset, data.to_vec())))
    //                         .expect("Send block back");
    //                 }
    //                 None => {
    //                     eprintln!("Agent receives unexpected block (piece_i: {piece_i}, offset: {offset})");
    //                 }
    //             }
    //         }
    //         5 => {
    //             eprintln!("[Agent] received bitfield");
    //             let interested_msg = PeerMessage {
    //                 length: u32::to_be_bytes(1),
    //                 kind: 2,
    //                 content: vec![],
    //             };
    //             self.stream
    //                 .send(interested_msg)
    //                 .await
    //                 .expect("Send the interested message");
    //
    //             self.bitfield_tx
    //                 .send(Some(Bitfield::new(msg.content)))
    //                 .unwrap();
    //         }
    //         0 => {
    //             eprintln!("[Agent] received Choked");
    //             self.status_tx.send(Some(Status::Choked)).unwrap();
    //         }
    //         1 => {
    //             eprintln!("[Agent] received Unchoked");
    //             self.status_tx.send(Some(Status::Unchoked)).unwrap();
    //         }
    //         a => {
    //             eprintln!("[Agent] Unknown message type: {a}");
    //         }
    //     }
    // }
    // async fn receive_from_peer(&mut self) {
    //     if let Some(msg) = self.stream.next().await {
    //         let msg = msg.expect("Read peer message");
    //         match msg.kind {
    //             7 => {
    //                 let (piece_i, rest) = msg.content.split_at(4);
    //                 let (offset, data) = rest.split_at(4);
    //
    //                 let piece_i = u32::from_be_bytes(piece_i.try_into().expect("Extract piece index")) as usize;
    //                 let offset = u32::from_be_bytes(offset.try_into().expect("Extract block offset")) as usize;
    //
    //                 eprintln!("[Agent] Got piece: {piece_i}, block: {offset}");
    //
    //                 match self.block_senders.remove(&(piece_i, offset)) {
    //                     Some(sender) => {
    //                         sender
    //                             .send(Ok((piece_i, offset, data.to_vec())))
    //                             .expect("Send block back");
    //                     }
    //                     None => {
    //                         eprintln!("Agent receives unexpected block (piece_i: {piece_i}, offset: {offset})");
    //                     }
    //                 }
    //             }
    //             5 => {
    //                 eprintln!("[Agent] received bitfield");
    //                 let interested_msg = PeerMessage {
    //                     length: u32::to_be_bytes(1),
    //                     kind: 2,
    //                     content: vec![],
    //                 };
    //                 self.stream
    //                     .send(interested_msg)
    //                     .await
    //                     .expect("Send the interested message");
    //
    //                 self.bitfield_tx
    //                     .send(Some(Bitfield::new(msg.content)))
    //                     .unwrap();
    //             }
    //             0 => {
    //                 eprintln!("[Agent] received Choked");
    //                 self.status_tx.send(Some(Status::Choked)).unwrap();
    //             }
    //             1 => {
    //                 eprintln!("[Agent] received Unchoked");
    //                 self.status_tx.send(Some(Status::Unchoked)).unwrap();
    //             }
    //             a => {
    //                 eprintln!("[Agent] Unknown message type: {a}");
    //             }
    //         }
    //     }
    // }

    // fn extract_block_data(&self, msg: PeerMessage) -> anyhow::Result<(usize, usize, Vec<u8>)> {
    //     if msg.kind != 7 {
    //         anyhow::bail!("Expect a block response");
    //     }
    //     let (index, rest) = msg.content.split_at(4);
    //     let index = u32::from_be_bytes(index.try_into().expect("Parse piece index"));
    //
    //     let (block_offset, data) = rest.split_at(4);
    //     let block_offset =
    //         u32::from_be_bytes(block_offset.try_into().expect("Parse block offset")) as usize;
    //     Ok((index as usize, block_offset, data.to_vec()))
    // }
    // if let Some(msg) = stream.next().await {
    //     let msg = msg.expect("Read bitfield message");
    //     assert_eq!(msg.kind, 5);
    //     bitfield_tx
    //         .send(Some(Bitfield::new(msg.content)))
    //         .context("Send back bitfield")?;
    // }
    //
    // // Send an interested message
    // let interested_msg = PeerMessage {
    //     length: u32::to_be_bytes(1),
    //     kind: 2,
    //     content: vec![],
    // };
    // stream
    //     .send(interested_msg)
    //     .await
    //     .expect("Send the interested message");
    //
    // // Read an unchoke message
    // if let Some(msg) = stream.next().await {
    //     let msg = msg.expect("Read the unchoke message");
    //     assert_eq!(msg.kind, 1);
    // }
}
