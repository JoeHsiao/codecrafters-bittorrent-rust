extern crate core;

use codecrafters_bittorrent::tracker::TrackerResponse;
use core::fmt;
use fmt::Display;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_bencode;
use serde_bytes::ByteBuf;
use serde_json;
use sha1::{Digest, Sha1};
use std::fmt::Formatter;
use std::path::Path;
use std::str::FromStr;
use std::{env, fs};
use urlencoding;

#[derive(Deserialize, Serialize)]
struct Torrent {
    /// The URL of the tracker
    announce: String,
    info: Info,
}

impl Torrent {
    fn from_file(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let torrent_bytes = fs::read(path).expect("Read the torrent file");
        serde_bencode::from_bytes(&torrent_bytes).expect("Deserialize the torrent file")
    }
    fn info(&self) -> Vec<u8> {
        serde_bencode::to_bytes(&self.info).expect("Serialize info")
    }
    fn info_sha1(&self) -> [u8; 20] {
        Sha1::digest(&self.info()).into()
    }

    fn info_sha1_hex(&self) -> String {
        hex::encode(self.info_sha1())
    }
}
#[derive(Deserialize, Serialize)]
struct Info {
    /// In the single file case, name is the name of a file
    /// In the multiple file case, it's the name of a directory.
    /// It is purely advisory.
    name: String,

    /// piece length maps to the number of bytes in each piece the file is split into.
    #[serde(rename = "piece length")]
    piece_length: usize,

    /// pieces maps to a string whose length is a multiple of 20.
    /// It is to be subdivided into strings of length 20, each of which is the SHA1 hash of
    /// the piece at the corresponding index.
    pieces: ByteBuf,

    /// There is also a key length or a key files, but not both or neither.
    /// If length is present then the download represents a single file, otherwise it represents
    /// a set of files which go in a directory structure.
    #[serde(flatten)]
    files: FileList,
}
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum FileList {
    SingleFile { length: usize },
    MultiFile { files: Vec<FileDetails> },
}
#[derive(Deserialize, Serialize)]
struct FileDetails {
    length: usize,
    path: Vec<String>,
}

impl Display for FileList {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FileList::SingleFile { length } => {
                write!(f, "{length}")
            }
            FileList::MultiFile { .. } => {
                todo!()
            }
        }
    }
}
#[allow(dead_code)]
fn decode_bencoded_value(encoded_value: &str) -> (serde_json::Value, &str) {
    let mut chars = encoded_value.chars();
    match chars.next() {
        Some('l') => {
            let mut rest = chars.as_str();
            let mut result = Vec::new();
            while !rest.is_empty() && rest.chars().next() != Some('e') {
                let (value, remaining) = decode_bencoded_value(rest);
                result.push(value);
                rest = remaining;
            }
            (
                result.into(),
                rest.strip_prefix("e").expect("List does not end with 'e'"),
            )
        }
        Some('i') => {
            let (num, rest) = chars
                .as_str()
                .split_once('e')
                .expect("Missing 'e' in a number");
            if num.contains('.') {
                (
                    num.parse::<f64>().expect("Invalid floating number").into(),
                    rest,
                )
            } else {
                (num.parse::<i64>().expect("Invalid number").into(), rest)
            }
        }
        Some('0'..='9') => {
            let Some((len, rest)) = encoded_value.split_once(':') else {
                panic!("String encoding does not contain ':'");
            };
            let len = len.parse::<usize>().expect("length is not an int");
            let (string, rest) = rest.split_at(len);
            (string.into(), rest)
        }
        Some(_) => {
            panic!("Unhandled encoded value: {}", encoded_value);
        }
        None => (serde_json::Value::Null, chars.as_str()),
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    if command == "decode" {
        // You can use print statements as follows for debugging, they'll be visible when running tests.
        eprintln!("Logs from your program will appear here!");

        // Uncomment this block to pass the first stage
        let encoded_value = &args[2];
        let (decoded_value, _) = decode_bencoded_value(encoded_value);
        println!("{}", decoded_value.to_string());
    } else if command == "info" {
        let f = &args[2];
        let torrent = Torrent::from_file(f);
        let mut chunks = torrent.info.pieces.chunks_exact(20);

        println!("Track URL: {}", torrent.announce);
        println!("Length: {}", torrent.info.files);
        println!("Info Hash: {}", torrent.info_sha1_hex());
        println!("Piece Length: {}", torrent.info.piece_length);
        println!("Piece Hashes:");
        while let Some(c) = chunks.next() {
            println!("{}", hex::encode(c));
        }
    } else if command == "peers" {
        let f = &args[2];
        let torrent = Torrent::from_file(f);

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

        for bytes in tracker_res.peers.chunks_exact(6) {
            let (ip, port) = bytes.split_at(4);
            println!(
                "{}.{}.{}.{}:{}",
                ip[0],
                ip[1],
                ip[2],
                ip[3],
                u16::from_be_bytes([port[0], port[1]])
            );
        }

    } else {
        println!("unknown command: {}", args[1])
    }
}

