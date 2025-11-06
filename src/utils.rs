use anyhow::{Context, Result};
use std::io::SeekFrom;
use std::iter;
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

pub fn split_piece_for_files(
    files: Vec<(PathBuf, usize, usize)>,
    piece: Vec<u8>,
) -> impl Iterator<Item = (PathBuf, usize, Vec<u8>)> {
    let mut head = 0;
    let mut files = files.into_iter();

    iter::from_fn(move || match files.next() {
        Some((file, offset, size)) => {
            assert!(
                head + size <= piece.len(),
                "piece length not long enough to split into files"
            );
            let result = Some((file, offset, piece[head..size].to_vec()));
            head += size;
            result
        }
        None => None,
    })
}
pub async fn write_to_file(path: &Path, offset: usize, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(path)
        .await
        .context("Open file")?;

    file.seek(SeekFrom::Start(offset as u64))
        .await
        .context("Seek file offset")?;

    file.write_all(&data).await.context("Write to file")?;
    Ok(())
}
