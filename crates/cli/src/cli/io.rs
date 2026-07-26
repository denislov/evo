use std::io::{self, Read};

pub(crate) const MAX_STDIN_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn read_text_from(mut reader: impl Read, max_bytes: usize) -> io::Result<String> {
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    reader
        .by_ref()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("input exceeds the {max_bytes} byte safety limit"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_stops_after_limit_plus_one() {
        let input = vec![b'x'; 33];
        let error = read_text_from(input.as_slice(), 32).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
