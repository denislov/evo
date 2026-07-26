pub fn ansi_sequence_len(text: &str, byte_pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if byte_pos >= bytes.len() || bytes[byte_pos] != 0x1b {
        return None;
    }

    let next = *bytes.get(byte_pos + 1)?;
    match next {
        b'[' => csi_len(bytes, byte_pos),
        b']' => string_sequence_len(bytes, byte_pos),
        b'_' => string_sequence_len(bytes, byte_pos),
        _ => None,
    }
}

fn csi_len(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start + 2;
    while pos < bytes.len() {
        let b = bytes[pos];
        if (0x40..=0x7e).contains(&b) {
            return Some(pos + 1 - start);
        }
        pos += 1;
    }
    None
}

fn string_sequence_len(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start + 2;
    while pos < bytes.len() {
        if bytes[pos] == 0x07 {
            return Some(pos + 1 - start);
        }
        if bytes[pos] == 0x1b && bytes.get(pos + 1) == Some(&b'\\') {
            return Some(pos + 2 - start);
        }
        pos += 1;
    }
    None
}
