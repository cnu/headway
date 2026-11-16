//! A small fuzzy matcher for the command palette.
//!
//! Every character of the query must appear in the text, in order (any case). Among
//! the ways to do that the best one is picked, so a match scores higher for starting
//! the text, starting a word, running unbroken and leaving few gaps. It's a few dozen
//! lines instead of a dependency: the lists are small and the ranking is ours to tune.

/// The longest text looked at (the rest of a very long line can't matter for a match).
const MAX_CHARS: usize = 200;

const MATCH: i32 = 10;
/// Bonus when the character follows the previous matched one directly.
const CONSECUTIVE: i32 = 14;
/// Cost of each character skipped between two matches.
const GAP: i32 = 2;
/// Cost of each character skipped before the first match (at most `LEAD_MAX`).
const LEAD: i32 = 1;
const LEAD_MAX: usize = 6;
const START: i32 = 15;
const WORD_START: i32 = 10;
const CAMEL: i32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    /// Which characters of the text matched (character indexes, not bytes).
    pub positions: Vec<usize>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn bonus(t: &[char], i: usize) -> i32 {
    if i == 0 {
        START
    } else if !t[i - 1].is_alphanumeric() {
        WORD_START
    } else if t[i - 1].is_lowercase() && t[i].is_uppercase() {
        CAMEL
    } else {
        0
    }
}

/// A query ready to be matched against many texts (the query is read once).
pub struct Pattern {
    q: Vec<char>,
}

impl Pattern {
    pub fn new(query: &str) -> Self {
        Self { q: query.chars().filter(|c| !c.is_whitespace()).map(lower).collect() }
    }

    /// Match against `text`. Spaces in the query are ignored. `None` when some
    /// character of the query can't be found in order; an empty query matches everything.
    pub fn find(&self, text: &str) -> Option<Match> {
        let q = &self.q;
        if q.is_empty() {
            return Some(Match { score: 0, positions: Vec::new() });
        }
        // Most texts fail here, without allocating anything.
        let mut want = q.iter();
        let mut next = want.next();
        for c in text.chars().take(MAX_CHARS) {
            if next == Some(&lower(c)) {
                next = want.next();
                if next.is_none() {
                    break;
                }
            }
        }
        if next.is_some() {
            return None;
        }
        let t: Vec<char> = text.chars().take(MAX_CHARS).collect();
        let (n, m) = (t.len(), q.len());
        const NONE: i32 = i32::MIN / 2;
        // dp[j][i]: the best score with query[j] matched at text[i]; from[j][i]: where query[j-1] was.
        let mut dp = vec![vec![NONE; n]; m];
        let mut from = vec![vec![usize::MAX; n]; m];
        for j in 0..m {
            // The best earlier match to jump to over a gap, as `score + GAP * k` for text[k].
            let (mut run, mut run_k) = (NONE, usize::MAX);
            for i in 0..n {
                if j > 0 && i >= 2 {
                    let k = i - 2;
                    if dp[j - 1][k] > NONE && dp[j - 1][k] + GAP * k as i32 > run {
                        run = dp[j - 1][k] + GAP * k as i32;
                        run_k = k;
                    }
                }
                if lower(t[i]) != q[j] {
                    continue;
                }
                let here = MATCH + bonus(&t, i);
                if j == 0 {
                    dp[0][i] = here - LEAD * i.min(LEAD_MAX) as i32;
                    continue;
                }
                let (mut best, mut best_k) = (NONE, usize::MAX);
                if i >= 1 && dp[j - 1][i - 1] > NONE {
                    best = dp[j - 1][i - 1] + CONSECUTIVE;
                    best_k = i - 1;
                }
                if run > NONE && run - GAP * (i as i32 - 1) > best {
                    best = run - GAP * (i as i32 - 1);
                    best_k = run_k;
                }
                if best > NONE {
                    dp[j][i] = best + here;
                    from[j][i] = best_k;
                }
            }
        }
        // The match that ends best; an earlier tie wins. Shorter texts are a little better.
        let (mut end, mut top) = (usize::MAX, NONE);
        for (i, &s) in dp[m - 1].iter().enumerate() {
            if s > top {
                top = s;
                end = i;
            }
        }
        if end == usize::MAX {
            return None;
        }
        let mut positions = vec![0; m];
        let mut i = end;
        for j in (0..m).rev() {
            positions[j] = i;
            i = from[j][i];
        }
        Some(Match { score: top - (n as i32) / 8, positions })
    }
}

/// Match `query` against `text` once (see [`Pattern`] for many texts).
#[cfg(test)]
pub fn fuzzy(query: &str, text: &str) -> Option<Match> {
    Pattern::new(query).find(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, t: &str) -> i32 {
        fuzzy(q, t).unwrap_or_else(|| panic!("{q:?} should match {t:?}")).score
    }

    #[test]
    fn needs_every_character_in_order() {
        assert!(fuzzy("pll", "Pull").is_some());
        assert!(fuzzy("lup", "Pull").is_none(), "order matters");
        assert!(fuzzy("pullx", "Pull").is_none());
        assert!(fuzzy("longer than the text", "short").is_none());
    }

    #[test]
    fn ignores_case_and_spaces() {
        assert_eq!(fuzzy("PULL", "pull"), fuzzy("pull", "pull"));
        assert_eq!(fuzzy("st all", "Stage all").map(|m| m.positions), fuzzy("stall", "Stage all").map(|m| m.positions));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy("", "anything"), Some(Match { score: 0, positions: vec![] }));
        assert_eq!(fuzzy("  ", "anything").map(|m| m.score), Some(0));
    }

    #[test]
    fn prefix_beats_word_start_beats_middle() {
        let prefix = score("pu", "Push");
        let word = score("pu", "Stash pop and push");
        let middle = score("pu", "Compute");
        assert!(prefix > word, "{prefix} > {word}");
        assert!(word > middle, "{word} > {middle}");
    }

    #[test]
    fn unbroken_runs_beat_scattered_letters() {
        assert!(score("sta", "Stage all") > score("sta", "Show the all"));
        assert!(score("fetch", "Fetch") > score("fetch", "Find every tag, check history"));
    }

    #[test]
    fn word_initials_work_and_camel_case_counts() {
        let m = fuzzy("sa", "Stage all").unwrap();
        assert_eq!(m.positions, vec![0, 6], "S of Stage and a of all");
        assert!(score("hc", "HeadwayConfig") > score("hc", "ahxcz"));
    }

    #[test]
    fn a_hundred_thousand_paths_take_a_blink() {
        let paths: Vec<String> = (0..100_000).map(|i| format!("crates/module{}/src/feature_{}/handler_{}.rs", i % 97, i % 1013, i)).collect();
        let started = std::time::Instant::now();
        for q in ["h", "hand", "mod7 hand 99"] {
            let p = Pattern::new(q);
            let n = paths.iter().filter_map(|t| p.find(t)).count();
            assert!(n > 0);
        }
        let took = started.elapsed();
        eprintln!("three searches over 100000 paths: {took:?}");
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn shorter_text_wins_a_tie() {
        assert!(score("push", "Push") > score("push", "Push to the remote"));
    }

    #[test]
    fn positions_are_character_indexes() {
        let m = fuzzy("é", "café").unwrap();
        assert_eq!(m.positions, vec![3]);
        let m = fuzzy("main", "src/main.rs").unwrap();
        assert_eq!(m.positions, vec![4, 5, 6, 7]);
    }
}
