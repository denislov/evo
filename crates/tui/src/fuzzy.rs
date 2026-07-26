#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let primary = match_query(&query_lower, &text_lower);
    if primary.matches {
        return primary;
    }

    let Some(swapped_query) = swapped_alpha_numeric_query(&query_lower) else {
        return primary;
    };

    let swapped = match_query(&swapped_query, &text_lower);
    if !swapped.matches {
        return primary;
    }

    FuzzyMatch {
        matches: true,
        score: swapped.score + 5.0,
    }
}

pub fn fuzzy_filter_indices<T, F, S>(items: &[T], query: &str, get_text: F) -> Vec<usize>
where
    F: Fn(&T) -> S,
    S: AsRef<str>,
{
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return (0..items.len()).collect();
    }

    let mut results = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let text = get_text(item);
        let mut total_score = 0.0;
        let mut all_match = true;
        for token in &tokens {
            let matched = fuzzy_match(token, text.as_ref());
            if matched.matches {
                total_score += matched.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((index, total_score));
        }
    }

    results.sort_by(|(left_index, left_score), (right_index, right_score)| {
        left_score
            .total_cmp(right_score)
            .then_with(|| left_index.cmp(right_index))
    });
    results.into_iter().map(|(index, _)| index).collect()
}

fn match_query(query: &str, text: &str) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.chars().count() > text.chars().count() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let query_chars = query.chars().collect::<Vec<_>>();
    let text_chars = text.chars().collect::<Vec<_>>();
    let mut query_index = 0;
    let mut score = 0.0;
    let mut last_match_index: Option<usize> = None;
    let mut consecutive_matches = 0.0;

    for (index, ch) in text_chars.iter().copied().enumerate() {
        if query_index >= query_chars.len() {
            break;
        }

        if ch == query_chars[query_index] {
            let is_word_boundary = index == 0
                || text_chars
                    .get(index - 1)
                    .is_some_and(|prev| is_boundary(*prev));

            if last_match_index == Some(index.saturating_sub(1)) {
                consecutive_matches += 1.0;
                score -= consecutive_matches * 5.0;
            } else {
                consecutive_matches = 0.0;
                if let Some(last) = last_match_index {
                    score += ((index - last - 1) as f64) * 2.0;
                }
            }

            if is_word_boundary {
                score -= 10.0;
            }

            score += (index as f64) * 0.1;
            last_match_index = Some(index);
            query_index += 1;
        }
    }

    if query_index < query_chars.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if query == text {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

fn is_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '/' | ':')
}

fn swapped_alpha_numeric_query(query: &str) -> Option<String> {
    if query.is_empty() {
        return None;
    }

    let split = query
        .char_indices()
        .find_map(|(index, ch)| ch.is_ascii_digit().then_some(index));
    if let Some(split) = split {
        let (letters, digits) = query.split_at(split);
        if !letters.is_empty()
            && !digits.is_empty()
            && letters.chars().all(|ch| ch.is_ascii_alphabetic())
            && digits.chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(format!("{digits}{letters}"));
        }
    }

    let split = query
        .char_indices()
        .find_map(|(index, ch)| ch.is_ascii_alphabetic().then_some(index));
    if let Some(split) = split {
        let (digits, letters) = query.split_at(split);
        if !digits.is_empty()
            && !letters.is_empty()
            && digits.chars().all(|ch| ch.is_ascii_digit())
            && letters.chars().all(|ch| ch.is_ascii_alphabetic())
        {
            return Some(format!("{letters}{digits}"));
        }
    }

    None
}
