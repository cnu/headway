//! Syntax colours for diffs and blame.
//!
//! A whole file is highlighted at once (so a string or comment spanning many
//! lines comes out right), on a worker thread, and kept per blob id: the same
//! version of a file is only ever highlighted once. Lines are tagged with the
//! kind of token each part is; [`crate::theme::syntax`] picks the colours.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::git;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Files bigger than this aren't highlighted.
pub const MAX_BYTES: usize = 1024 * 1024;
/// Nor files with a line longer than this (minified code: slow, and unreadable anyway).
pub const MAX_LINE_BYTES: usize = 4000;
/// How many highlighted files are kept.
const CACHE_FILES: usize = 200;

/// What kind of text a part of a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    Plain,
    Comment,
    String,
    Escape,
    Number,
    Constant,
    Keyword,
    Type,
    Function,
    Tag,
    Attribute,
    Macro,
}

/// One part of a line: bytes `start..end` are a `Tok`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub tok: Tok,
}

/// A highlighted file: the parts of each line (lines numbered from 0).
#[derive(Debug, Default)]
pub struct Highlighted {
    pub lines: Vec<Vec<Span>>,
    /// Length of each line in bytes, to check a line is the one highlighted.
    lens: Vec<u32>,
}

impl Highlighted {
    /// The parts of line `n` (from 1), if it's `text`. Anything else (a line the
    /// diff shows differently from the file) gets no colours rather than wrong ones.
    pub fn line(&self, n: u32, text: &str) -> Option<&[Span]> {
        let i = (n as usize).checked_sub(1)?;
        (self.lens.get(i).copied() == Some(text.len() as u32)).then(|| self.lines[i].as_slice())
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

/// The language of a file, from its name or (for scripts) its first line.
fn syntax_for(path: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
    let found = ss
        .find_syntax_by_extension(name)
        .or_else(|| (!ext.is_empty()).then(|| ss.find_syntax_by_extension(ext)).flatten())
        .or_else(|| (!ext.is_empty()).then(|| ss.find_syntax_by_extension(&ext.to_ascii_lowercase())).flatten())
        .or_else(|| ss.find_syntax_by_first_line(first_line))?;
    // Plain text has nothing to colour.
    (found.name != "Plain Text").then_some(found)
}

/// Whether Headway knows how to colour this file.
pub fn knows(path: &str) -> bool {
    syntax_for(path, "").is_some()
}

/// Which token a stack of scopes is, looking from the innermost scope out.
fn classify(stack: &[Scope], memo: &mut HashMap<Scope, Option<Tok>>) -> Tok {
    for s in stack.iter().rev() {
        let t = *memo.entry(*s).or_insert_with(|| tok_of(&s.build_string()));
        if let Some(t) = t {
            return t;
        }
    }
    Tok::Plain
}

fn tok_of(scope: &str) -> Option<Tok> {
    let has = |p: &str| scope == p || scope.starts_with(&format!("{p}."));
    Some(if has("comment") {
        Tok::Comment
    } else if has("constant.character.escape") {
        Tok::Escape
    } else if has("string") || has("markup.raw") {
        Tok::String
    } else if has("constant.numeric") {
        Tok::Number
    } else if has("constant") || has("variable.language") || has("support.constant") {
        Tok::Constant
    } else if has("entity.name.tag") {
        Tok::Tag
    } else if has("entity.other.attribute-name") || has("meta.attribute") || has("markup.heading") {
        Tok::Attribute
    } else if has("support.function.macro") || has("entity.name.macro") || has("meta.preprocessor") {
        Tok::Macro
    } else if has("entity.name.function") || has("support.function") || has("variable.function") {
        Tok::Function
    } else if has("entity.name.type")
        || has("entity.name.class")
        || has("entity.name.struct")
        || has("entity.name.enum")
        || has("entity.name.trait")
        || has("entity.other.inherited-class")
        || has("support.type")
        || has("support.class")
        || has("storage.type.primitive")
        || has("storage.type.numeric")
    {
        Tok::Type
    } else if has("keyword.operator") || has("punctuation") {
        // Operators and punctuation take the colour around them (a string's quotes
        // are string), and are plain elsewhere: colour on every symbol is noise.
        return None;
    } else if has("keyword") || has("storage") {
        Tok::Keyword
    } else {
        return None;
    })
}

/// Colour a whole file. `None` if it's too big or of an unknown language.
pub fn highlight(path: &str, text: &str) -> Option<Highlighted> {
    if text.len() > MAX_BYTES {
        return None;
    }
    let first = text.lines().next().unwrap_or("");
    let syntax = syntax_for(path, first)?;
    if text.lines().any(|l| l.len() > MAX_LINE_BYTES) {
        return None;
    }
    let ss = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut memo = HashMap::new();
    let mut out = Highlighted::default();
    // `split_inclusive` keeps each line's "\n", which the grammars expect.
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let ops = state.parse_line(raw, ss).ok()?;
        let mut spans: Vec<Span> = Vec::new();
        let mut at = 0usize;
        let push = |spans: &mut Vec<Span>, from: usize, to: usize, tok: Tok| {
            let to = to.min(line.len());
            if from >= to {
                return;
            }
            match spans.last_mut() {
                Some(last) if last.tok == tok && last.end as usize == from => last.end = to as u32,
                _ => spans.push(Span { start: from as u32, end: to as u32, tok }),
            }
        };
        for (pos, op) in ops {
            let tok = classify(stack.as_slice(), &mut memo);
            push(&mut spans, at, pos, tok);
            at = pos;
            let _ = stack.apply(&op);
        }
        let tok = classify(stack.as_slice(), &mut memo);
        push(&mut spans, at, line.len(), tok);
        spans.retain(|s| s.tok != Tok::Plain);
        out.lines.push(spans);
        out.lens.push(line.len() as u32);
    }
    Some(out)
}

struct Cache {
    map: HashMap<String, Option<Arc<Highlighted>>>,
    order: VecDeque<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache { map: HashMap::new(), order: VecDeque::new() }))
}

/// The colours of blob `id` (named `path`), highlighting it with the text from
/// `read` if it hasn't been yet. Keyed by blob id and language, since the same
/// contents under another name may be another language.
pub fn cached(id: &str, path: &str, read: impl FnOnce() -> Option<String>) -> Option<Arc<Highlighted>> {
    let lang = syntax_for(path, "").map(|s| s.name.as_str()).unwrap_or("?");
    let key = format!("{id}:{lang}");
    if let Some(hit) = cache().lock().ok()?.map.get(&key) {
        return hit.clone();
    }
    let done = read().and_then(|text| highlight(path, &text)).map(Arc::new);
    if let Ok(mut c) = cache().lock() {
        if c.map.insert(key.clone(), done.clone()).is_none() {
            c.order.push_back(key);
        }
        while c.order.len() > CACHE_FILES {
            if let Some(old) = c.order.pop_front() {
                c.map.remove(&old);
            }
        }
    }
    done
}

/// Whether blob `id` (as `path`) has been highlighted already.
pub fn is_cached(id: &str, path: &str) -> bool {
    let lang = syntax_for(path, "").map(|s| s.name.as_str()).unwrap_or("?");
    cache().lock().is_ok_and(|c| c.map.contains_key(&format!("{id}:{lang}")))
}

/// The colours of both sides of one changed file.
#[derive(Debug, Default)]
pub struct Sides {
    pub old: Option<Arc<Highlighted>>,
    pub new: Option<Arc<Highlighted>>,
}

/// One file to highlight in a [`Job`].
pub struct JobFile {
    pub path: String,
    pub old_path: String,
    pub old_id: Option<String>,
    pub new_id: Option<String>,
    /// The new side is the file in the working folder (not yet in git).
    pub new_on_disk: bool,
}

/// Highlighting for a whole diff, done after the diff is on screen: each file's
/// colours go into its slot as they're ready.
pub struct Job {
    pub repo: PathBuf,
    pub slots: Arc<Vec<OnceLock<Sides>>>,
    pub files: Vec<(usize, JobFile)>,
}

impl Job {
    /// Do the work (slow: call it in the background). `progress` is called now
    /// and then while it runs, to get the colours drawn. Stops early if the
    /// diff is closed.
    pub fn run(self, progress: &dyn Fn()) {
        // Read every blob not highlighted yet at once.
        let wanted: Vec<String> = self
            .files
            .iter()
            .flat_map(|(_, f)| [f.old_id.as_ref().map(|i| (i, &f.old_path)), f.new_id.as_ref().filter(|_| !f.new_on_disk).map(|i| (i, &f.path))])
            .flatten()
            .filter(|(id, path)| !is_cached(id, path))
            .map(|(id, _)| id.clone())
            .collect();
        let mut texts = if wanted.is_empty() { HashMap::new() } else { git::small_blobs(&self.repo, &wanted, MAX_BYTES as u64).unwrap_or_default() };
        let mut last = Instant::now();
        for (fi, f) in self.files {
            // Nobody holds the diff any more: it was closed or replaced.
            if Arc::strong_count(&self.slots) == 1 {
                return;
            }
            let old = f.old_id.as_ref().and_then(|id| cached(id, &f.old_path, || texts.remove(id)));
            let new = f.new_id.as_ref().and_then(|id| {
                cached(id, &f.path, || {
                    if f.new_on_disk {
                        let full = self.repo.join(&f.path);
                        let small = std::fs::metadata(&full).is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES as u64);
                        small.then(|| std::fs::read(&full).ok()).flatten().map(|b| String::from_utf8_lossy(&b).into_owned())
                    } else {
                        texts.remove(id)
                    }
                })
            });
            if let Some(slot) = self.slots.get(fi) {
                let _ = slot.set(Sides { old, new });
            }
            if last.elapsed() >= Duration::from_millis(100) {
                progress();
                last = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(h: &Highlighted, line: usize, text: &str) -> Vec<(String, Tok)> {
        h.lines[line].iter().map(|s| (text[s.start as usize..s.end as usize].to_string(), s.tok)).collect()
    }

    #[test]
    fn knows_languages_by_name_and_first_line() {
        assert!(knows("src/main.rs"));
        assert!(knows("web/app.ts"), "two-face adds TypeScript");
        assert!(knows("Cargo.toml"));
        assert!(knows("Makefile"));
        assert!(!knows("notes.txt"));
        assert!(!knows("data.unknownext"));
        assert!(highlight("run", "#!/bin/sh\necho hi\n").is_some(), "scripts are found by their #! line");
    }

    #[test]
    fn strings_and_comments_carry_across_lines() {
        let src = "/* a\n   b */\nfn main() {\n    let s = \"x\n y\";\n}\n";
        let h = highlight("a.rs", src).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        assert_eq!(toks(&h, 1, lines[1]), vec![("   b */".into(), Tok::Comment)], "inside a block comment begun on the line before");
        assert!(toks(&h, 2, lines[2]).contains(&("fn".into(), Tok::Keyword)));
        assert!(toks(&h, 2, lines[2]).contains(&("main".into(), Tok::Function)));
        assert_eq!(toks(&h, 4, lines[4]), vec![(" y\"".into(), Tok::String)], "the second line of a string is still string");
    }

    #[test]
    fn lines_are_checked_before_use_and_crlf_is_fine() {
        let h = highlight("a.py", "x = 1\r\n# hi\r\n").unwrap();
        assert!(h.line(2, "# hi").is_some());
        assert!(h.line(2, "# hello").is_none(), "a line that doesn't match gets no colours");
        assert!(h.line(3, "").is_none());
        assert!(h.line(0, "").is_none());
    }

    #[test]
    fn huge_or_minified_files_are_skipped() {
        assert!(highlight("a.js", &"x".repeat(MAX_BYTES + 1)).is_none());
        assert!(highlight("a.js", &format!("var a = '{}';\n", "x".repeat(MAX_LINE_BYTES))).is_none());
    }

    #[test]
    fn the_cache_reads_each_blob_once() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        let mut reads = 0;
        for _ in 0..3 {
            let h = cached(id, "x.rs", || {
                reads += 1;
                Some("fn a() {}\n".into())
            });
            assert!(h.is_some());
        }
        assert_eq!(reads, 1);
        assert!(is_cached(id, "x.rs"));
    }
}
