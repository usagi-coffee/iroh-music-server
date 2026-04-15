use std::fs::File;
use std::io::{Read, Result};
use std::path::Path;

pub fn read_file_prefix(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; max_bytes];
    let bytes_read = file.read(&mut buffer)?;
    buffer.truncate(bytes_read);
    Ok(buffer)
}
