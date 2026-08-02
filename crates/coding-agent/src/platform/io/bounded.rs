use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub(crate) fn read_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input path is not a regular file",
        ));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(too_large(max_bytes));
    }
    read_from(file, max_bytes)
}

pub(crate) fn read_text(path: &Path, max_bytes: usize) -> io::Result<String> {
    let bytes = read_file(path, max_bytes)?;
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_from(mut reader: impl Read, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    reader
        .by_ref()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(bytes)
}

fn too_large(max_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("input exceeds the {max_bytes} byte safety limit"),
    )
}
