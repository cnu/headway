//! How the lines of a hunk relate to each other: which removed line became which
//! added line (for side-by-side rows), and which words of a changed line changed.
//!
//! Everything here works on the parsed diff ([`crate::git::DiffLine`]), the same
//! lines that line and hunk staging use, so what's drawn is what gets staged.

use std::ops::Range;

use similar::{Algorithm, DiffTag, capture_diff_slices};

use crate::git::{DiffLine, LineKind};

/// Lines longer than this (in bytes) get no word-level marks: the comparison
/// costs too much and the marks would be lost in the line anyway.
pub const MAX_WORD_DIFF_BYTES: usize = 1000;
/// Below this share of unchanged text, two lines count as rewritten rather than
/// edited, and get no word-level marks.
const MIN_SHARED: f32 = 0.25;

/// One row of the side-by-side view: the lines (indexes into the hunk) shown on
/// the left (before) and on the right (after). An unchanged line is on both sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aligned {
    pub old: Option<usize>,
    pub new: Option<usize>,
}

/// Line up a hunk's lines in rows: each block of removed lines faces the block
/// of added lines that follows it, first with first; the longer block's extra
/// lines face a gap. "\ No newline at end of file" stays with the line it's about.
pub fn align(lines: &[DiffLine]) -> Vec<Aligned> {
    let mut out = Vec::with_capacity(lines.len());
    let (mut dels, mut adds): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    let flush = |out: &mut Vec<Aligned>, dels: &mut Vec<usize>, adds: &mut Vec<usize>| {
        for k in 0..dels.len().max(adds.len()) {
            out.push(Aligned { old: dels.get(k).copied(), new: adds.get(k).copied() });
        }
        dels.clear();
        adds.clear();
    };
    for (i, l) in lines.iter().enumerate() {
        match l.kind {
            LineKind::Del => {
                // Removed lines after added ones start a new block.
                if !adds.is_empty() {
                    flush(&mut out, &mut dels, &mut adds);
                }
                dels.push(i);
            }
            LineKind::Add => adds.push(i),
            LineKind::Context => {
                flush(&mut out, &mut dels, &mut adds);
                out.push(Aligned { old: Some(i), new: Some(i) });
            }
            LineKind::NoNewline => {
                let before = i.checked_sub(1);
                if before.is_some() && adds.last().copied() == before {
                    adds.push(i);
                } else if before.is_some() && dels.last().copied() == before {
                    dels.push(i);
                } else {
                    flush(&mut out, &mut dels, &mut adds);
                    out.push(Aligned { old: Some(i), new: Some(i) });
                }
            }
        }
    }
    flush(&mut out, &mut dels, &mut adds);
    out
}

/// The removed and added lines that face each other (one became the other).
pub fn pairs(lines: &[DiffLine]) -> impl Iterator<Item = (usize, usize)> + '_ {
    align(lines).into_iter().filter_map(|a| match (a.old, a.new) {
        (Some(o), Some(n)) if lines[o].kind == LineKind::Del && lines[n].kind == LineKind::Add => Some((o, n)),
        _ => None,
    })
}

/// One line's changed words (byte ranges) and whether only whitespace changed.
pub type LineMarks = (Vec<Range<usize>>, bool);

/// What changed between two facing lines: byte ranges of each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    pub old: Vec<Range<usize>>,
    pub new: Vec<Range<usize>>,
    /// Only spaces, tabs or line endings changed (drawn so they can be seen).
    pub whitespace_only: bool,
}

/// Split a line into words (letters, digits, `_`), runs of whitespace, and single
/// other characters: code changes at that grain (`foo(bar)` → `foo(baz)`).
fn tokens(s: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum K {
        Word,
        Space,
        Other,
    }
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            K::Word
        } else if c.is_whitespace() {
            K::Space
        } else {
            K::Other
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<K> = None;
    for (i, c) in s.char_indices() {
        let k = kind(c);
        match out.last_mut() {
            Some(r) if last.as_ref() == Some(&k) && k != K::Other => r.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

fn push_merged(v: &mut Vec<Range<usize>>, r: Range<usize>) {
    if r.is_empty() {
        return;
    }
    match v.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => v.push(r),
    }
}

/// The words that differ between a removed line and the added line facing it.
/// `None` when there's nothing useful to mark: a line is very long, or the two
/// have too little in common to call one an edit of the other.
pub fn word_diff(old: &str, new: &str) -> Option<Marks> {
    if old.len() > MAX_WORD_DIFF_BYTES || new.len() > MAX_WORD_DIFF_BYTES {
        return None;
    }
    let (a, b) = (tokens(old), tokens(new));
    let (sa, sb): (Vec<&str>, Vec<&str>) = (a.iter().map(|r| &old[r.clone()]).collect(), b.iter().map(|r| &new[r.clone()]).collect());
    let span = |toks: &[Range<usize>], r: Range<usize>| if r.is_empty() { 0..0 } else { toks[r.start].start..toks[r.end - 1].end };
    let mut m = Marks::default();
    let mut shared = 0;
    for op in capture_diff_slices(Algorithm::Myers, &sa, &sb) {
        let (tag, ro, rn) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => shared += ro.clone().map(|i| sa[i].trim().len()).sum::<usize>(),
            DiffTag::Delete => push_merged(&mut m.old, span(&a, ro)),
            DiffTag::Insert => push_merged(&mut m.new, span(&b, rn)),
            DiffTag::Replace => {
                push_merged(&mut m.old, span(&a, ro));
                push_merged(&mut m.new, span(&b, rn));
            }
        }
    }
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    m.whitespace_only = old != new && squash(old) == squash(new);
    let longest = squash(old).len().max(squash(new).len());
    if !m.whitespace_only && longest > 0 && (shared as f32) < MIN_SHARED * longest as f32 {
        return None;
    }
    Some(m)
}

/// Word-level marks for every line of a hunk that has them (by line index).
pub fn hunk_marks(lines: &[DiffLine]) -> Vec<(usize, LineMarks)> {
    let mut out = Vec::new();
    for (o, n) in pairs(lines) {
        if let Some(m) = word_diff(&lines[o].text, &lines[n].text) {
            out.push((o, (m.old, m.whitespace_only)));
            out.push((n, (m.new, m.whitespace_only)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine { kind, old_no: None, new_no: None, text: text.into() }
    }

    fn marked<'a>(s: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|r| &s[r.clone()]).collect()
    }

    #[test]
    fn word_diff_marks_just_the_changed_words() {
        let (a, b) = ("let total = price * count;", "let total = price * quantity;");
        let m = word_diff(a, b).unwrap();
        assert_eq!(marked(a, &m.old), vec!["count"]);
        assert_eq!(marked(b, &m.new), vec!["quantity"]);
        assert!(!m.whitespace_only);
    }

    #[test]
    fn word_diff_splits_on_punctuation_and_merges_neighbours() {
        let (a, b) = ("call(a, b)", "call(a, b, c)");
        let m = word_diff(a, b).unwrap();
        assert!(m.old.is_empty(), "nothing was taken away");
        assert_eq!(marked(b, &m.new), vec![", c"], "the comma, space and word added together are one mark");
        let (a, b) = ("x.foo()", "x.bar()");
        assert_eq!(marked(b, &word_diff(a, b).unwrap().new), vec!["bar"]);
    }

    #[test]
    fn word_diff_handles_unicode() {
        let (a, b) = ("naïve café", "naïve crème");
        let m = word_diff(a, b).unwrap();
        assert_eq!(marked(a, &m.old), vec!["café"]);
        assert_eq!(marked(b, &m.new), vec!["crème"]);
    }

    #[test]
    fn whitespace_only_changes_are_told_apart() {
        let (a, b) = ("\tif x {", "    if x {");
        let m = word_diff(a, b).unwrap();
        assert!(m.whitespace_only);
        assert_eq!(marked(a, &m.old), vec!["\t"]);
        assert_eq!(marked(b, &m.new), vec!["    "]);
        let m = word_diff("a = 1", "a = 1 ").unwrap();
        assert!(m.whitespace_only, "trailing space");
        assert_eq!(m.new, vec![5..6]);
    }

    #[test]
    fn rewritten_and_very_long_lines_get_no_marks() {
        assert_eq!(word_diff("fn parse(input: &str)", "// totally different text here"), None);
        let long = "x".repeat(MAX_WORD_DIFF_BYTES + 1);
        assert_eq!(word_diff(&long, "x"), None);
        assert_eq!(word_diff("same", "same"), Some(Marks::default()), "identical lines have nothing to mark");
    }

    #[test]
    fn align_faces_removed_lines_with_added_ones() {
        use LineKind::*;
        // 0 ctx, 1-2 del, 3 add, 4 ctx, 5 add, 6 del, 7 add, 8 add
        let lines = [
            line(Context, "a"),
            line(Del, "b"),
            line(Del, "c"),
            line(Add, "B"),
            line(Context, "d"),
            line(Add, "e"),
            line(Del, "f"),
            line(Add, "F"),
            line(Add, "G"),
        ];
        let rows: Vec<(Option<usize>, Option<usize>)> = align(&lines).iter().map(|a| (a.old, a.new)).collect();
        assert_eq!(
            rows,
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(3)),
                (Some(2), None),
                (Some(4), Some(4)),
                (None, Some(5)), // added lines before any removal stand alone
                (Some(6), Some(7)),
                (None, Some(8)),
            ]
        );
        assert_eq!(pairs(&lines).collect::<Vec<_>>(), vec![(1, 3), (6, 7)]);
        // Every line shows up exactly once on each side it belongs to.
        let mut seen: Vec<usize> = align(&lines).iter().flat_map(|a| [a.old, a.new.filter(|n| Some(*n) != a.old)]).flatten().collect();
        seen.sort();
        assert_eq!(seen, (0..lines.len()).collect::<Vec<_>>());
    }

    #[test]
    fn align_keeps_no_newline_markers_with_their_line() {
        use LineKind::*;
        let lines = [line(Del, "a"), line(NoNewline, " No newline at end of file"), line(Add, "a"), line(NoNewline, " No newline at end of file")];
        let rows: Vec<(Option<usize>, Option<usize>)> = align(&lines).iter().map(|a| (a.old, a.new)).collect();
        assert_eq!(rows, vec![(Some(0), Some(2)), (Some(1), Some(3))]);
        assert_eq!(pairs(&lines).collect::<Vec<_>>(), vec![(0, 2)], "the markers aren't a pair of changed lines");
        let ctx = [line(Context, "z"), line(NoNewline, " No newline at end of file")];
        assert_eq!(align(&ctx).len(), 2);
    }

    #[test]
    fn hunk_marks_cover_both_lines_of_each_pair() {
        use LineKind::*;
        let lines = [line(Del, "old_name(1)"), line(Add, "new_name(1)"), line(Add, "extra()")];
        let m = hunk_marks(&lines);
        let name = || std::iter::once(0..8).collect::<Vec<_>>();
        assert_eq!(m, vec![(0, (name(), false)), (1, (name(), false))]);
    }
}
