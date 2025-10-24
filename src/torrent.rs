use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use sha1::{Digest, Sha1};

#[derive(Deserialize, Serialize, Debug)]
pub struct Torrent {
    /// The URL of the tracker
    pub announce: String,
    pub info: Info,
}

impl Torrent {
    pub fn from_file(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let torrent_bytes = fs::read(path).expect("Read the torrent file");
        serde_bencode::from_bytes(&torrent_bytes).expect("Deserialize the torrent file")
    }
    pub fn info(&self) -> Vec<u8> {
        serde_bencode::to_bytes(&self.info).expect("Serialize info")
    }
    pub fn info_sha1(&self) -> [u8; 20] {
        Sha1::digest(&self.info()).into()
    }

    pub fn info_sha1_hex(&self) -> String {
        hex::encode(self.info_sha1())
    }
}
#[derive(Deserialize, Serialize, Debug)]
pub struct Info {
    /// In the single file case, name is the name of a file
    /// In the multiple file case, it's the name of a directory.
    /// It is purely advisory.
    pub name: String,

    /// piece length maps to the number of bytes in each piece the file is split into.
    #[serde(rename = "piece length")]
    pub piece_length: usize,

    /// pieces maps to a string whose length is a multiple of 20.
    /// It is to be subdivided into strings of length 20, each of which is the SHA1 hash of
    /// the piece at the corresponding index.
    pub pieces: ByteBuf,

    /// There is also a key length or a key files, but not both or neither.
    /// If length is present then the download represents a single file, otherwise it represents
    /// a set of files which go in a directory structure.
    #[serde(flatten)]
    pub files: FileList,
}
#[derive(Deserialize, Serialize, Debug)]
#[serde(untagged)]
pub enum FileList {
    SingleFile { length: usize },
    MultiFile { files: Vec<FileDetails> },
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

#[derive(Deserialize, Serialize, Debug)]
pub struct FileDetails {
    length: usize,
    path: Vec<String>,
}