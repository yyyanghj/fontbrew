#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SearchMatchScore {
    tier: u8,
    distance: usize,
    target_len: usize,
}

impl SearchMatchScore {
    fn exact(target_len: usize) -> Self {
        Self {
            tier: 0,
            distance: 0,
            target_len,
        }
    }

    fn prefix(distance: usize, target_len: usize) -> Self {
        Self {
            tier: 1,
            distance,
            target_len,
        }
    }

    fn contains(distance: usize, target_len: usize) -> Self {
        Self {
            tier: 2,
            distance,
            target_len,
        }
    }

    fn subsequence(distance: usize, target_len: usize) -> Self {
        Self {
            tier: 3,
            distance,
            target_len,
        }
    }

    fn typo(distance: usize, target_len: usize) -> Self {
        Self {
            tier: 4,
            distance,
            target_len,
        }
    }
}

pub(crate) fn best_search_match_score<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<SearchMatchScore> {
    let query = normalize_search_text(query);
    if query.is_empty() {
        return Some(SearchMatchScore::exact(0));
    }

    candidates
        .into_iter()
        .filter_map(|candidate| search_match_score(&query, candidate))
        .min()
}

fn search_match_score(query: &str, candidate: &str) -> Option<SearchMatchScore> {
    let candidate = normalize_search_text(candidate);
    if candidate.is_empty() {
        return None;
    }

    let query_len = query.chars().count();
    let candidate_len = candidate.chars().count();

    if candidate == query {
        return Some(SearchMatchScore::exact(candidate_len));
    }

    if candidate.starts_with(query) {
        return Some(SearchMatchScore::prefix(
            candidate_len - query_len,
            candidate_len,
        ));
    }

    if candidate.contains(query) {
        return Some(SearchMatchScore::contains(
            candidate_len - query_len,
            candidate_len,
        ));
    }

    if query_len < 3 {
        return None;
    }

    if is_subsequence(query, &candidate) {
        return Some(SearchMatchScore::subsequence(
            candidate_len - query_len,
            candidate_len,
        ));
    }

    let max_distance = typo_distance_limit(query_len);
    bounded_levenshtein_distance(query, &candidate, max_distance)
        .map(|distance| SearchMatchScore::typo(distance, candidate_len))
}

fn normalize_search_text(input: &str) -> String {
    input
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn is_subsequence(query: &str, candidate: &str) -> bool {
    let mut query_chars = query.chars();
    let Some(mut next_query_char) = query_chars.next() else {
        return true;
    };

    for candidate_char in candidate.chars() {
        if candidate_char != next_query_char {
            continue;
        }

        let Some(next_char) = query_chars.next() else {
            return true;
        };
        next_query_char = next_char;
    }

    false
}

fn typo_distance_limit(query_len: usize) -> usize {
    match query_len {
        0..=2 => 0,
        3..=4 => 1,
        5..=7 => 2,
        _ => 3,
    }
}

fn bounded_levenshtein_distance(left: &str, right: &str, max_distance: usize) -> Option<usize> {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();

    if left.len().abs_diff(right.len()) > max_distance {
        return None;
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_minimum = current[0];

        for (right_index, right_char) in right.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution_cost);
            row_minimum = row_minimum.min(current[right_index + 1]);
        }

        if row_minimum > max_distance {
            return None;
        }

        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right.len()];
    (distance <= max_distance).then_some(distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(query: &str, candidate: &str) -> bool {
        best_search_match_score(query, [candidate]).is_some()
    }

    #[test]
    fn search_match_ignores_case_spacing_and_punctuation() {
        assert!(matches("source sans 3", "source-sans-3"));
        assert!(matches("MAPLE MONO", "Maple Mono NF CN"));
    }

    #[test]
    fn search_match_accepts_ordered_abbreviations_and_small_typos() {
        assert!(matches("lra", "Lora"));
        assert!(matches("sorce sans", "Source Sans 3"));
        assert!(matches("scpro", "Source Code Pro"));
    }

    #[test]
    fn search_match_keeps_very_short_queries_conservative() {
        assert!(!matches("lr", "Lora"));
        assert!(matches("lo", "Lora"));
    }
}
