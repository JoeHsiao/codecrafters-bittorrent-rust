use serde_json;
use serde::{Deserialize};
use std::env;
use serde_bencode;

#[derive(Deserialize)]
struct Torrent {
    /// The URL of the tracker
    announce: String,
    info: Info,
}

#[derive(Deserialize)]
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
    pieces: Vec<u8>,

    /// There is also a key length or a key files, but not both or neither.
    /// If length is present then the download represents a single file, otherwise it represents
    /// a set of files which go in a directory structure.
    #[serde(flatten)]
    files: FileList,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(untagged)]
enum FileList {
    SingleFile {
        length: usize,
    },
    MultiFile {
        files: Vec<FileDetails>,
    },
}
#[derive(Deserialize)]
struct FileDetails {
    length: usize,
    path: Vec<String>,
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
            (result.into(), rest.strip_prefix("e").expect("List does not end with 'e'"))
        }
        Some('i') => {
            let (num, rest) = chars.as_str()
                .split_once('e')
                .expect("Missing 'e' in a number");
            if num.contains('.') {
                (num.parse::<f64>().expect("Invalid floating number").into(), rest)
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
        Some(_) => { panic!("Unhandled encoded value: {}", encoded_value); }
        None => { (serde_json::Value::Null, chars.as_str()) }
    }
}

// Usage: your_program.sh decode "<encoded_value>"
fn main() {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    if command == "decode" {
        // You can use print statements as follows for debugging, they'll be visible when running tests.
        eprintln!("Logs from your program will appear here!");

        // Uncomment this block to pass the first stage
        let encoded_value = &args[2];
        let (decoded_value, _) = decode_bencoded_value(encoded_value);
        println!("{}", decoded_value.to_string());
    } else {
        println!("unknown command: {}", args[1])
    }
}
