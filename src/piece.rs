#[derive(Debug)]
pub struct Piece {
    bytes: Vec<u8>,
    writes: usize,
}
impl Piece {
    pub fn new(size: usize) -> Self {
        Piece {
            bytes: vec![0; size],
            writes: 0,
        }
    }
    pub fn write(&mut self, offset: usize, chunk: &[u8]) -> anyhow::Result<()> {
        let end = offset + chunk.len();
        if end > self.bytes.len() {
            anyhow::bail!("Cannot fit chunk into piece");
        }
        self.bytes[offset..end].copy_from_slice(chunk);
        self.writes += chunk.len();
        Ok(())
    }
    pub fn bytes(self) -> Vec<u8> {
        self.bytes
    }
    pub fn expected_length(&self) -> usize {
        self.bytes.len()
    }
    pub fn writes(&self) -> usize {
        self.writes
    }
    pub fn done(&self) -> bool {
        self.writes == self.expected_length()
    }
}