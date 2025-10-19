extern crate core;
use serde_json;
use std::env;
use codecrafters_bittorrent::commands::{handshake, peers};
use codecrafters_bittorrent::torrent::Torrent;

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
        let peers = peers(&args[2]).await;
        for peer in peers {
            println!("{peer}");
        }
    } else if command == "handshake" {
        println!("{}", handshake(&args[2], &args[3]).await);
    } else {
        println!("unknown command: {}", args[1])
    }
}
