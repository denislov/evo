pub fn find_word_backward(text: &str, cursor: usize) -> usize {
    let mut boundary = 0;
    let mut in_word = false;
    for (index, ch) in text[..cursor].char_indices().rev() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() {
            if in_word {
                return index + ch.len_utf8();
            }
        } else {
            in_word = true;
            boundary = index;
        }
    }
    boundary
}

pub fn find_word_forward(text: &str, cursor: usize) -> usize {
    let mut seen_word = false;
    for (offset, ch) in text[cursor..].char_indices() {
        if ch.is_whitespace() || ch.is_ascii_punctuation() {
            if seen_word {
                return cursor + offset;
            }
        } else {
            seen_word = true;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::{find_word_backward, find_word_forward};

    #[test]
    fn finds_simple_word_boundaries() {
        let text = "alpha beta";
        assert_eq!(find_word_backward(text, text.len()), 6);
        assert_eq!(find_word_forward(text, 0), 5);
    }
}
