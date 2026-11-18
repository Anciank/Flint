/// Fuzzy match scoring and match positions for Quick Open and Symbol Search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i64,
    pub positions: Vec<usize>,
}

/// Scores `candidate` against `query` (case-insensitive subsequence match).
/// Returns `None` if `query` is not a subsequence of `candidate`.
/// When `query` is empty, matches with score 0 and empty positions.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }

    let q_chars: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let c_chars: Vec<(usize, char)> = candidate.char_indices().collect();
    if q_chars.len() > c_chars.len() {
        return None;
    }

    let mut positions = Vec::with_capacity(q_chars.len());
    let mut qi = 0;
    let mut prev_match_ci: Option<usize> = None;
    let mut score: i64 = 0;

    for (ci, &(byte_idx, ch)) in c_chars.iter().enumerate() {
        if qi >= q_chars.len() {
            break;
        }
        let lower = ch.to_lowercase().next().unwrap_or(ch);
        if lower == q_chars[qi] {
            positions.push(byte_idx);
            let mut bonus: i64 = 10;
            if ci == 0 {
                bonus += 25;
            }
            if let Some(prev) = prev_match_ci {
                if ci == prev + 1 {
                    bonus += 18;
                } else {
                    score -= ((ci - prev - 1) as i64).min(12);
                }
            }
            if ch.is_uppercase() {
                bonus += 12;
            }
            if ci > 0 {
                let prev_ch = c_chars[ci - 1].1;
                if matches!(prev_ch, '/' | '\\' | '_' | '-' | '.' | ':' | ' ') {
                    bonus += 20;
                }
            }
            score += bonus;
            prev_match_ci = Some(ci);
            qi += 1;
        }
    }

    if qi < q_chars.len() {
        return None;
    }

    if candidate.eq_ignore_ascii_case(query) {
        score += 100;
    } else if candidate
        .to_lowercase()
        .starts_with(&query.to_lowercase())
    {
        score += 45;
    } else if candidate.to_lowercase().contains(&query.to_lowercase()) {
        score += 20;
    }

    // Prefer shorter, tighter matches
    score -= (c_chars.len() as i64) / 3;

    Some(FuzzyMatch { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_exact_and_boundary_matches() {
        let exact = fuzzy_match("FimEngine", "FimEngine").unwrap();
        let prefix = fuzzy_match("FimEngine", "FimEngineConfig").unwrap();
        let loose = fuzzy_match("FimEngine", "f_i_m_e_n_g_i_n_e").unwrap();
        assert!(exact.score > prefix.score);
        assert!(prefix.score > loose.score);
    }

    #[test]
    fn returns_none_when_not_subsequence() {
        assert!(fuzzy_match("xyz", "abc").is_none());
    }
}
