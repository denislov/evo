use unicode_normalization::UnicodeNormalization;

pub(crate) fn normalize_unicode_confusables(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed: String = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{00A0}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{2002}'..='\u{200A}' => ' ',
            other => other,
        })
        .collect()
}

pub(crate) fn seek_unique_sequence(
    lines: &[String],
    pattern: &[&str],
    start: usize,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() || start > lines.len().saturating_sub(pattern.len()) {
        return None;
    }
    let comparators: [fn(&str) -> String; 4] = [
        str::to_owned,
        |line| line.trim_end().to_owned(),
        |line| line.trim().to_owned(),
        normalize_unicode_confusables,
    ];
    for normalize in comparators {
        let candidates = (start..=lines.len().saturating_sub(pattern.len()))
            .filter(|index| {
                lines[*index..*index + pattern.len()]
                    .iter()
                    .zip(pattern)
                    .all(|(actual, expected)| normalize(actual) == normalize(expected))
            })
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [index] => return Some(*index),
            [] => {}
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_sequence_degrades_in_a_fixed_fail_closed_order() {
        assert_eq!(
            seek_unique_sequence(&["value  ".into()], &["value"], 0),
            Some(0)
        );
        assert_eq!(
            seek_unique_sequence(&["  value".into()], &["value"], 0),
            Some(0)
        );
        assert_eq!(
            seek_unique_sequence(&["“value”".into()], &["\"value\""], 0),
            Some(0)
        );
        assert_eq!(
            seek_unique_sequence(&["same".into(), "same".into()], &["same"], 0),
            None
        );
    }
}
