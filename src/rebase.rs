//! Interactive rebase: the plan the editor builds, the todo list git is given,
//! and the two hidden commands git runs in place of an editor.
//!
//! Everything here is pure (no git), so it's cheap to test. `git.rs` runs the
//! rebase itself.
//!
//! git starts `GIT_SEQUENCE_EDITOR` once, to let you change the todo list, and
//! `GIT_EDITOR` whenever it wants a commit message. Both are Headway itself:
//! `headway --write-rebase-todo <our todo> <git's todo>` copies ours over git's,
//! and `headway --write-rebase-message <session folder> <git's message file>`
//! writes the message chosen in the editor. Headway answers by looking at which
//! step git is running (the last line of `rebase-merge/done`), not by counting
//! calls, because git also opens the editor for a commit resumed after a conflict.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What to do with one commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl Act {
    pub const ALL: [Act; 6] = [Act::Pick, Act::Reword, Act::Edit, Act::Squash, Act::Fixup, Act::Drop];

    /// The word in git's todo list.
    pub fn word(self) -> &'static str {
        match self {
            Act::Pick => "pick",
            Act::Reword => "reword",
            Act::Edit => "edit",
            Act::Squash => "squash",
            Act::Fixup => "fixup",
            Act::Drop => "drop",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Act::Pick => "Keep the commit as it is",
            Act::Reword => "Keep the commit and change its message",
            Act::Edit => "Stop after this commit so you can change it",
            Act::Squash => "Meld into the commit above and combine their messages",
            Act::Fixup => "Meld into the commit above and keep only its message",
            Act::Drop => "Leave the commit out",
        }
    }

    /// Squash and fixup melt into the row above.
    pub fn attaches(self) -> bool {
        matches!(self, Act::Squash | Act::Fixup)
    }
}

/// One commit in the editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub hash: String,
    pub summary: String,
    /// The full message as it will be committed (used by Reword).
    pub message: String,
    pub original: String,
    pub act: Act,
    /// Local branches (full names) that point at this commit and should move along.
    pub branches: Vec<String>,
    /// A remote branch already has this commit.
    pub pushed: bool,
}

impl Step {
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(8)]
    }
}

/// A commit with the squash and fixup rows melted into it (dropped rows aren't part of one).
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// Indexes into [`Plan::steps`], the head first.
    pub rows: Vec<usize>,
    pub has_squash: bool,
}

impl Chain {
    pub fn head(&self) -> usize {
        self.rows[0]
    }
    pub fn last(&self) -> usize {
        *self.rows.last().unwrap_or(&self.rows[0])
    }
}

/// A squash message the user edited, valid while the chain has these members.
#[derive(Clone, Debug, PartialEq)]
struct Combined {
    members: Vec<String>,
    text: String,
}

/// What the editor is editing: the commits oldest first, and how to run them.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Hashes in the order they were loaded, to tell whether anything changed.
    original: Vec<String>,
    edited: HashMap<String, Combined>,
    /// HEAD (commit and full branch name) when the plan was made: a plan for other commits mustn't run.
    pub taken_at: Option<(String, Option<String>)>,
}

impl Plan {
    pub fn new(steps: Vec<Step>) -> Plan {
        let original = steps.iter().map(|s| s.hash.clone()).collect();
        Plan { steps, original, edited: HashMap::new(), taken_at: None }
    }

    /// The rows that stay, grouped.
    pub fn chains(&self) -> Vec<Chain> {
        let mut out: Vec<Chain> = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            match s.act {
                Act::Drop => {}
                a if a.attaches() => {
                    if let Some(c) = out.last_mut() {
                        c.rows.push(i);
                        c.has_squash |= a == Act::Squash;
                    }
                }
                _ => out.push(Chain { rows: vec![i], has_squash: false }),
            }
        }
        out
    }

    /// (commits now, commits after the rebase).
    pub fn counts(&self) -> (usize, usize) {
        (self.steps.len(), self.chains().len())
    }

    /// Why this can't run, if it can't.
    pub fn problem(&self) -> Option<String> {
        let Some(first) = self.steps.iter().position(|s| s.act != Act::Drop) else {
            return Some("Every commit is dropped. Keep at least one.".into());
        };
        if self.steps[first].act.attaches() {
            return Some(format!("“{}” has no commit above it to melt into. Move it down or pick it.", self.steps[first].summary));
        }
        // git won't commit an empty message: it would stop the rebase.
        let m = self.messages();
        if let Some(s) = self.steps.iter().find(|s| m.rewords.get(&s.hash).is_some_and(|t| t.trim().is_empty())) {
            return Some(format!("The new message for “{}” is empty.", s.summary));
        }
        if m.squashes.values().any(|t| t.trim().is_empty()) {
            return Some("The message for a combined commit is empty.".into());
        }
        None
    }

    /// The oldest commit as it was loaded: the rebase starts from its parent.
    pub fn first_original(&self) -> Option<&str> {
        self.original.first().map(|s| s.as_str())
    }

    /// Nothing would change: same order, every commit picked.
    pub fn is_unchanged(&self) -> bool {
        self.steps.iter().all(|s| s.act == Act::Pick) && self.steps.iter().map(|s| &s.hash).eq(self.original.iter())
    }

    /// Is the first row the one a squash or fixup couldn't attach to?
    pub fn can_attach(&self, i: usize) -> bool {
        (0..i).any(|j| self.steps[j].act != Act::Drop)
    }

    /// Move a row (drag and drop): it ends up at index `to`.
    pub fn move_to(&mut self, from: usize, to: usize) {
        if from >= self.steps.len() || to >= self.steps.len() || from == to {
            return;
        }
        let s = self.steps.remove(from);
        self.steps.insert(to, s);
    }

    /// The message a chain's commit gets: the kept commits' messages one after
    /// the other (a fixup's is left out), unless the user changed it.
    pub fn combined(&self, chain: &Chain) -> String {
        let members: Vec<String> = chain.rows.iter().map(|&i| self.steps[i].hash.clone()).collect();
        if let Some(e) = self.edited.get(&self.steps[chain.last()].hash)
            && e.members == members
        {
            return e.text.clone();
        }
        let mut parts = Vec::new();
        for (n, &i) in chain.rows.iter().enumerate() {
            let s = &self.steps[i];
            if n == 0 || s.act == Act::Squash {
                // A message typed for a reword only counts while the row is one.
                let text = if s.act == Act::Reword { &s.message } else { &s.original };
                parts.push(text.trim().to_string());
            }
        }
        parts.join("\n\n")
    }

    pub fn set_combined(&mut self, chain: &Chain, text: String) {
        let members = chain.rows.iter().map(|&i| self.steps[i].hash.clone()).collect();
        self.edited.insert(self.steps[chain.last()].hash.clone(), Combined { members, text });
    }

    /// A reword at the head of a chain that has a squash is just a pick: the
    /// chain's message is the one that counts.
    fn effective(&self, i: usize, heads_squash: &[usize]) -> Act {
        let a = self.steps[i].act;
        if a == Act::Reword && heads_squash.contains(&i) { Act::Pick } else { a }
    }

    fn squash_heads(&self) -> Vec<usize> {
        self.chains().iter().filter(|c| c.has_squash).map(|c| c.head()).collect()
    }

    /// git's todo list for this plan. `update_refs`: add the `update-ref` lines
    /// that move each branch to where its commit ends up.
    pub fn todo(&self, update_refs: bool) -> String {
        let heads = self.squash_heads();
        let mut out = String::new();
        // Dropped commits first: they do nothing wherever they are, and git's
        // "missing commits" check just wants them mentioned.
        for s in self.steps.iter().filter(|s| s.act == Act::Drop) {
            out.push_str(&format!("drop {} {}\n", s.hash, one_line(&s.summary)));
        }
        let mut pending: Vec<&str> = Vec::new();
        let flush = |out: &mut String, pending: &mut Vec<&str>| {
            for b in pending.drain(..) {
                if update_refs {
                    out.push_str(&format!("update-ref {b}\n"));
                }
            }
        };
        for (i, s) in self.steps.iter().enumerate() {
            if s.act == Act::Drop {
                pending.extend(s.branches.iter().map(|b| b.as_str()));
                continue;
            }
            if !s.act.attaches() {
                flush(&mut out, &mut pending);
            }
            out.push_str(&format!("{} {} {}\n", self.effective(i, &heads).word(), s.hash, one_line(&s.summary)));
            pending.extend(s.branches.iter().map(|b| b.as_str()));
        }
        flush(&mut out, &mut pending);
        out
    }

    /// The messages git will ask for, by commit.
    pub fn messages(&self) -> Messages {
        let heads = self.squash_heads();
        let mut m = Messages::default();
        for (i, s) in self.steps.iter().enumerate() {
            if self.effective(i, &heads) == Act::Reword {
                m.rewords.insert(s.hash.clone(), s.message.trim_end().to_string());
            }
        }
        for c in self.chains().iter().filter(|c| c.has_squash) {
            m.squashes.insert(self.steps[c.last()].hash.clone(), self.combined(c).trim_end().to_string());
        }
        m
    }

    // -- autosquash ---------------------------------------------------------

    /// The row a `fixup!` or `squash!` commit at `i` is for, among the rows above it.
    fn autosquash_target(&self, i: usize) -> Option<usize> {
        let (_, target) = autosquash_subject(&self.steps[i].summary)?;
        // The oldest match, as git takes it (and never a dropped commit).
        (0..i).find(|&j| {
            let s = &self.steps[j];
            s.act != Act::Drop
                && (s.summary == target || (target.len() >= 7 && target.chars().all(|c| c.is_ascii_hexdigit()) && s.hash.starts_with(target)))
        })
    }

    /// How many `fixup!` / `squash!` commits could be moved under their commits.
    pub fn autosquash_count(&self) -> usize {
        (0..self.steps.len()).filter(|&i| self.steps[i].act == Act::Pick && self.autosquash_target(i).is_some()).count()
    }

    /// Put each `fixup!` / `squash!` commit under the commit it's for and melt
    /// it in, as `git rebase --autosquash` does.
    pub fn autosquash(&mut self) {
        let n = self.steps.len();
        for _ in 0..n {
            let Some(i) = (0..self.steps.len()).find(|&i| self.steps[i].act == Act::Pick && self.autosquash_target(i).is_some()) else { break };
            let t = self.autosquash_target(i).unwrap_or(0);
            let (kind, _) = autosquash_subject(&self.steps[i].summary).unwrap_or((Act::Fixup, ""));
            let mut s = self.steps.remove(i);
            s.act = kind;
            // After the target and whatever is already melted into it.
            let mut at = t + 1;
            while at < i && self.steps[at].act.attaches() {
                at += 1;
            }
            self.steps.insert(at, s);
        }
    }
}

/// `fixup! subject` -> (Fixup, "subject"); the prefix may repeat. `amend!` isn't handled.
fn autosquash_subject(summary: &str) -> Option<(Act, &str)> {
    let mut rest = summary;
    let mut kind = None;
    loop {
        if let Some(r) = rest.strip_prefix("fixup! ") {
            kind.get_or_insert(Act::Fixup);
            rest = r;
        } else if let Some(r) = rest.strip_prefix("squash! ") {
            kind.get_or_insert(Act::Squash);
            rest = r;
        } else {
            break;
        }
    }
    kind.map(|k| (k, rest))
}

fn one_line(s: &str) -> String {
    s.split(['\n', '\r']).next().unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// The session: what git needs to ask Headway for while a rebase runs

/// Messages by commit (full hash). `rewords`: the message of a reworded commit.
/// `squashes`: the message of a melted commit, by the last row of its chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Messages {
    pub rewords: BTreeMap<String, String>,
    pub squashes: BTreeMap<String, String>,
}

impl Messages {
    /// git removes lines starting with its comment character (`#`) from a message
    /// written by an editor. When one of ours has such a line, a character none of
    /// them starts a line with, for `core.commentChar` while the rebase runs.
    pub fn comment_char(&self) -> Option<char> {
        let all: Vec<&str> = self.rewords.values().chain(self.squashes.values()).flat_map(|m| m.lines()).collect();
        let starts = |c: char| all.iter().any(|l| l.starts_with(c));
        if !starts('#') {
            return None;
        }
        [';', '@', '!', '%', '|', '~', ':'].into_iter().find(|&c| !starts(c))
    }
}

/// What Headway remembers about the rebase it started, in a folder of the git
/// directory. It's only trusted while that rebase is the one running.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    /// Where HEAD was when it started (`rebase-merge/orig-head` says the same).
    pub orig_head: String,
    /// What Undo calls it.
    pub what: String,
    /// What the rebase is onto (`rebase-merge/onto`), when it isn't from the root: with
    /// `orig_head`, how a session tells its own rebase from another one.
    #[serde(default)]
    pub onto: Option<String>,
    /// The rebase's own folder, where the helper reads which step is running.
    pub rebase_merge: PathBuf,
    #[serde(default)]
    pub messages: Messages,
    /// `core.commentChar` for this rebase (see [`Messages::comment_char`]).
    #[serde(default)]
    pub comment_char: Option<char>,
    /// Fixup into…: the fixup commit made first, and what was HEAD before it.
    #[serde(default)]
    pub fixup: Option<(String, String)>,
    /// What to save for Undo, kept until the rebase finishes.
    #[serde(default)]
    pub undo: Option<crate::git::UndoPrep>,
}

pub const SESSION_FILE: &str = "session.json";
pub const TODO_FILE: &str = "todo";

// ---------------------------------------------------------------------------
// Commands

/// A path or word quoted for the `sh` git runs editors with. Backslashes become
/// slashes on Windows, where git's `sh` understands `C:/Program Files/…`.
pub fn sh_quote(p: &str) -> String {
    let p = if cfg!(windows) { p.replace('\\', "/") } else { p.to_string() };
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// The two editor commands, and any environment they need.
pub struct Editors {
    pub sequence: String,
    pub message: String,
    pub env: Vec<(String, String)>,
}

/// The commands for `GIT_SEQUENCE_EDITOR` and `GIT_EDITOR`. git adds the file as the last word.
pub fn editors(exe: &Path, todo: &Path, dir: &Path) -> Editors {
    let (exe, todo, dir) = (exe.to_string_lossy(), todo.to_string_lossy(), dir.to_string_lossy());
    #[cfg(not(test))]
    {
        Editors {
            sequence: format!("{} --write-rebase-todo {}", sh_quote(&exe), sh_quote(&todo)),
            message: format!("{} --write-rebase-message {}", sh_quote(&exe), sh_quote(&dir)),
            env: Vec::new(),
        }
    }
    // Tests run the test program, which doesn't know the flags: it runs a test
    // that does what the flag would.
    #[cfg(test)]
    {
        let me = format!("{} --exact rebase::tests::{{}} --nocapture", sh_quote(&exe));
        Editors {
            sequence: me.replace("{}", "helper_todo_entry"),
            message: me.replace("{}", "helper_message_entry"),
            env: vec![("HEADWAY_TEST_TODO".into(), todo.into_owned()), ("HEADWAY_TEST_DIR".into(), dir.into_owned())],
        }
    }
}

/// Run a hidden helper command. `None`: these arguments aren't one.
pub fn run_helper(args: &[String]) -> Option<Result<(), String>> {
    match args {
        [flag, ours, theirs] if flag == "--write-rebase-todo" => Some(std::fs::copy(ours, theirs).map(drop).map_err(|e| format!("Couldn't write the rebase todo list: {e}"))),
        [flag, dir, file] if flag == "--write-rebase-message" => Some(write_message(Path::new(dir), Path::new(file))),
        _ => None,
    }
}

/// A todo line's command (long or short form) and commit.
fn parse_line(line: &str) -> Option<(Act, &str)> {
    let mut w = line.split_whitespace();
    let act = match w.next()? {
        "pick" | "p" => Act::Pick,
        "reword" | "r" => Act::Reword,
        "edit" | "e" => Act::Edit,
        "squash" | "s" => Act::Squash,
        "fixup" | "f" => Act::Fixup,
        "drop" | "d" => Act::Drop,
        _ => return None,
    };
    Some((act, w.next()?))
}

fn matches_commit(full: &str, given: &str) -> bool {
    given.len() >= 4 && full.starts_with(given)
}

/// The message for what git is running now, if Headway has one.
fn message_for(session: &Session, done: &str, todo: &str) -> Option<String> {
    let (act, sha) = done.lines().rev().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).and_then(parse_line)?;
    match act {
        Act::Reword => session.messages.rewords.iter().find(|(h, _)| matches_commit(h, sha)).map(|(_, m)| m.clone()),
        Act::Squash | Act::Fixup => {
            // Only the last step of a chain asks for the message.
            let next = todo.lines().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).and_then(parse_line);
            if next.is_some_and(|(a, _)| a.attaches()) {
                return None;
            }
            session.messages.squashes.iter().find(|(h, _)| matches_commit(h, sha)).map(|(_, m)| m.clone())
        }
        _ => None,
    }
}

fn write_message(dir: &Path, file: &Path) -> Result<(), String> {
    // No session, or not one this rebase belongs to: leave git's message alone.
    let Ok(text) = std::fs::read_to_string(dir.join(SESSION_FILE)) else { return Ok(()) };
    let Ok(session) = serde_json::from_str::<Session>(&text) else { return Ok(()) };
    let done = std::fs::read_to_string(session.rebase_merge.join("done")).unwrap_or_default();
    let todo = std::fs::read_to_string(session.rebase_merge.join("git-rebase-todo")).unwrap_or_default();
    if let Some(m) = message_for(&session, &done, &todo) {
        std::fs::write(file, format!("{}\n", m.trim_end())).map_err(|e| format!("Couldn't write the commit message: {e}"))?;
    }
    Ok(())
}

/// The command git is running now, from the rebase's `done` file.
pub fn current_command(done: &str) -> Option<Act> {
    done.lines().rev().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).and_then(parse_line).map(|(a, _)| a)
}

/// (commit being applied, commits to apply) from a rebase's `done` and todo
/// files, counting only the lines that apply a commit.
pub fn progress(done: &str, todo: &str) -> (usize, usize) {
    let n = |t: &str| t.lines().filter_map(parse_line).filter(|(a, _)| *a != Act::Drop).count();
    let d = n(done);
    (d, d + n(todo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(hash: &str, summary: &str, act: Act) -> Step {
        Step { hash: format!("{hash:0<40}"), summary: summary.into(), message: summary.into(), original: summary.into(), act, branches: vec![], pushed: false }
    }

    fn plan(items: &[(&str, &str, Act)]) -> Plan {
        Plan::new(items.iter().map(|(h, s, a)| step(h, s, *a)).collect())
    }

    #[test]
    fn counts_chains_and_attachment() {
        let p = plan(&[("a", "A", Act::Pick), ("b", "B", Act::Squash), ("c", "C", Act::Fixup), ("d", "D", Act::Drop), ("e", "E", Act::Pick)]);
        assert_eq!(p.counts(), (5, 2));
        assert_eq!(p.chains()[0].rows, vec![0, 1, 2]);
        assert!(p.chains()[0].has_squash);
        let p = plan(&[("a", "A", Act::Pick), ("d", "D", Act::Drop), ("b", "B", Act::Fixup)]);
        assert!(p.can_attach(2) && p.can_attach(1) && !p.can_attach(0));
        let p = plan(&[("d", "D", Act::Drop), ("b", "B", Act::Pick)]);
        assert!(!p.can_attach(1));
        let p = plan(&[("a", "A", Act::Pick), ("d", "D", Act::Drop), ("b", "B", Act::Fixup)]);
        assert_eq!(p.chains()[0].rows, vec![0, 2]);
    }

    #[test]
    fn problems_and_unchanged() {
        let mut p = plan(&[("a", "A", Act::Squash), ("b", "B", Act::Pick)]);
        assert!(p.problem().unwrap().contains("no commit above"));
        p.steps[0].act = Act::Drop;
        assert_eq!(p.problem(), None);
        p.steps[1].act = Act::Drop;
        assert!(p.problem().unwrap().contains("Every commit is dropped"));
        let mut p = plan(&[("a", "A", Act::Reword), ("b", "B", Act::Squash)]);
        assert_eq!(p.problem(), None);
        let c = p.chains()[0].clone();
        p.set_combined(&c, "  ".into());
        assert!(p.problem().unwrap().contains("combined commit is empty"));
        let mut p = plan(&[("a", "A", Act::Reword)]);
        p.steps[0].message = "\n".into();
        assert!(p.problem().unwrap().contains("“A” is empty"));
        let mut p = plan(&[("a", "A", Act::Pick), ("b", "B", Act::Pick)]);
        assert!(p.is_unchanged());
        p.move_to(0, 1);
        assert!(!p.is_unchanged());
        p.move_to(1, 0);
        assert!(p.is_unchanged());
        p.steps[0].act = Act::Reword;
        assert!(!p.is_unchanged());
    }

    #[test]
    fn todo_has_drops_first_and_branches_after_their_chain() {
        let mut p = plan(&[("a", "A", Act::Pick), ("b", "B", Act::Fixup), ("c", "C", Act::Drop), ("d", "D", Act::Reword)]);
        p.steps[1].branches = vec!["refs/heads/mid".into()];
        p.steps[2].branches = vec!["refs/heads/gone".into()];
        let t = p.todo(true);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], format!("drop {:0<40} C", "c"));
        assert_eq!(lines[1], format!("pick {:0<40} A", "a"));
        assert_eq!(lines[2], format!("fixup {:0<40} B", "b"));
        // Both branches wait until the chain is over, then move to where it ended.
        assert_eq!(&lines[3..5], ["update-ref refs/heads/mid", "update-ref refs/heads/gone"]);
        assert_eq!(lines[5], format!("reword {:0<40} D", "d"));
        assert!(!p.todo(false).contains("update-ref"));
    }

    #[test]
    fn a_reword_heading_a_squash_is_a_pick_and_the_combined_message_is_sent() {
        let mut p = plan(&[("a", "first", Act::Reword), ("b", "second", Act::Squash), ("c", "third", Act::Fixup), ("d", "other", Act::Reword)]);
        p.steps[0].message = "first\n\nbody".into();
        assert!(p.todo(false).contains(&format!("pick {:0<40} first", "a")));
        assert_eq!(p.combined(&p.chains()[0]), "first\n\nbody\n\nsecond");
        let m = p.messages();
        assert_eq!(m.squashes[&format!("{:0<40}", "c")], "first\n\nbody\n\nsecond");
        assert_eq!(m.rewords.keys().collect::<Vec<_>>(), vec![&format!("{:0<40}", "d")]);
        // The user's own wording wins while the chain stays the same, and not after it changes.
        let c = p.chains()[0].clone();
        p.set_combined(&c, "mine".into());
        assert_eq!(p.messages().squashes[&format!("{:0<40}", "c")], "mine");
        p.steps[2].act = Act::Drop;
        assert!(p.messages().squashes.contains_key(&format!("{:0<40}", "b")));
        assert_ne!(p.messages().squashes[&format!("{:0<40}", "b")], "mine");
    }

    #[test]
    fn autosquash_moves_fixups_under_their_commits() {
        let mut p = plan(&[("a", "Add cart", Act::Pick), ("b", "Add search", Act::Pick), ("c", "fixup! Add cart", Act::Pick), ("d", "squash! Add search", Act::Pick), ("e", "fixup! fixup! Add cart", Act::Pick), ("f", "fixup! Nothing", Act::Pick)]);
        assert_eq!(p.autosquash_count(), 4 - 1);
        p.autosquash();
        let order: Vec<(&str, Act)> = p.steps.iter().map(|s| (s.summary.as_str(), s.act)).collect();
        assert_eq!(
            order,
            vec![
                ("Add cart", Act::Pick),
                ("fixup! Add cart", Act::Fixup),
                ("fixup! fixup! Add cart", Act::Fixup),
                ("Add search", Act::Pick),
                ("squash! Add search", Act::Squash),
                ("fixup! Nothing", Act::Pick),
            ]
        );
        assert_eq!(p.autosquash_count(), 0);
        // Two commits with one subject: the oldest, like git; and never a dropped one.
        let mut p = plan(&[("a", "Same", Act::Drop), ("b", "Same", Act::Pick), ("c", "Same", Act::Pick), ("d", "fixup! Same", Act::Pick)]);
        p.autosquash();
        assert_eq!(p.steps.iter().map(|s| (s.hash.as_str(), s.act)).map(|(h, a)| (h.chars().next().unwrap(), a)).collect::<Vec<_>>(), [('a', Act::Drop), ('b', Act::Pick), ('d', Act::Fixup), ('c', Act::Pick)]);
        // By commit id too.
        let mut p = plan(&[("abcdef0123", "Add cart", Act::Pick), ("b", "x", Act::Pick), ("c", "fixup! abcdef0", Act::Pick)]);
        p.autosquash();
        assert_eq!(p.steps[1].summary, "fixup! abcdef0");
    }

    #[test]
    fn progress_counts_only_commits() {
        let done = "drop 1111111 gone\npick 2222222 a\nupdate-ref refs/heads/x\nedit 3333333 b\n";
        let todo = "# comment\n\nfixup 4444444 c\nupdate-ref refs/heads/y\nexec echo\nreword 5555555 d\n";
        assert_eq!(progress(done, todo), (2, 4));
    }

    #[test]
    fn a_comment_character_is_chosen_only_when_a_message_needs_one() {
        let mut m = Messages::default();
        m.rewords.insert("a".into(), "Fix\n\nnormal text with a # in it".into());
        assert_eq!(m.comment_char(), None);
        m.rewords.insert("b".into(), "Fix\n\n#12 is the ticket\n;note".into());
        assert_eq!(m.comment_char(), Some('@'));
        assert_eq!(current_command("pick a b\nedit c d\n"), Some(Act::Edit));
        assert_eq!(current_command("pick a b\nfixup c d\n\n"), Some(Act::Fixup));
    }

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(sh_quote("/a b/it's"), "'/a b/it'\\''s'");
    }

    #[test]
    fn the_message_helper_follows_the_step_git_is_running() {
        let h = |s: &str| format!("{s:0<40}");
        let mut s = Session::default();
        s.messages.rewords.insert(h("aaaaaaaa"), "new words".into());
        s.messages.squashes.insert(h("cccccccc"), "combined".into());
        // A reword; a plain pick resumed after a conflict gets nothing.
        assert_eq!(message_for(&s, "pick 1234567 x\nreword aaaaaaaa y\n", ""), Some("new words".into()));
        assert_eq!(message_for(&s, "reword aaaaaaaa y\npick bbbbbbbb z\n", ""), None);
        // A squash chain answers at its last step only.
        assert_eq!(message_for(&s, "squash bbbbbbbb z\n", "fixup cccccccc w\n"), None);
        assert_eq!(message_for(&s, "squash bbbbbbbb z\nfixup cccccccc w\n", "update-ref refs/heads/x\npick dddddddd q\n"), Some("combined".into()));
        // Abbreviations and short command names.
        assert_eq!(message_for(&s, "r aaaaaaa y\n", ""), Some("new words".into()));
    }

    // The test program stands in for `headway --write-rebase-todo …`.
    #[test]
    fn helper_todo_entry() {
        if let (Ok(ours), Some(theirs)) = (std::env::var("HEADWAY_TEST_TODO"), std::env::args().next_back()) {
            run_helper(&["--write-rebase-todo".into(), ours, theirs]).expect("a helper command").expect("the helper works");
        }
    }

    // … and `headway --write-rebase-message …`.
    #[test]
    fn helper_message_entry() {
        if let (Ok(dir), Some(file)) = (std::env::var("HEADWAY_TEST_DIR"), std::env::args().next_back()) {
            run_helper(&["--write-rebase-message".into(), dir, file]).expect("a helper command").expect("the helper works");
        }
    }
}
