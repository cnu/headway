//! The operation log: every undoable action, saved per repository under the app's
//! data folder, so undo works after a restart too.
//!
//! The git side (what to save, how to put it back) is in [`crate::git`]; this keeps
//! the list and decides which entry may be undone.
//!
//! Not the command log ([`crate::cmdlog`]), which records every git command that ran;
//! this only keeps what is needed to take an action back.

use crate::git::{self, GitResult, UndoEntry, Undoable};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Entries kept per repository.
pub const KEEP: usize = 100;

/// Log files are read and written from worker threads.
static LOCK: Mutex<()> = Mutex::new(());

fn log_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("headway-undo-test-{}", std::process::id()));
    }
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("headway").join("undo")
}

/// One file per repository: its name, plus a hash of its full path to tell apart
/// repositories with the same name.
fn log_file(repo: &Path) -> PathBuf {
    // FNV-1a: stable across Rust versions, unlike the standard hasher.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in repo.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    let name: String = git::repo_name(repo).chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    log_dir().join(format!("{name}-{h:016x}.json"))
}

fn load(repo: &Path) -> Vec<UndoEntry> {
    std::fs::read_to_string(log_file(repo)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(repo: &Path, entries: &[UndoEntry]) -> Result<(), String> {
    let file = log_file(repo);
    std::fs::create_dir_all(log_dir()).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    // Write a copy and swap it in, so a crash never leaves half a file.
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())
}

/// Add an entry (giving it an id), keeping the newest [`KEEP`].
pub fn push(repo: &Path, mut entry: UndoEntry) -> Result<UndoEntry, String> {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut entries = load(repo);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    entry.id = now.max(entries.last().map_or(0, |e| e.id + 1));
    entries.push(entry.clone());
    let extra = entries.len().saturating_sub(KEEP);
    entries.drain(..extra);
    save(repo, &entries)?;
    Ok(entry)
}

/// The entry an undo would take back: the newest one not undone yet.
pub fn latest(repo: &Path) -> Option<UndoEntry> {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    load(repo).into_iter().rev().find(|e| !e.undone)
}

/// Run an action, saving what it changes so it can be undone. Returns its output and
/// its log entry (none when it changed nothing).
pub fn run_recorded(repo: &Path, what: &Undoable, label: &str, op: impl FnOnce(&Path) -> GitResult<String>) -> GitResult<(String, Option<UndoEntry>)> {
    // If the copies can't be made, don't run the action at all.
    let prep = git::prepare_undo(repo, what).map_err(|e| format!("Couldn't save what this changes, so it wasn't done.\n\n{e}"))?;
    let out = op(repo)?;
    let entry = match git::finish_undo(repo, prep, label) {
        Ok(Some(e)) => push(repo, e).ok(),
        _ => None,
    };
    Ok((out, entry))
}

/// Undo the newest entry. With `id`, only if that's still the newest one (a later
/// action may depend on it).
pub fn undo(repo: &Path, id: Option<u64>) -> GitResult<String> {
    let Some(entry) = latest(repo) else { return Err("There's nothing to undo.".into()) };
    if let Some(id) = id
        && entry.id != id
    {
        let entries = load(repo);
        return Err(match entries.iter().find(|e| e.id == id) {
            Some(e) if e.undone => "That has already been undone.".into(),
            _ => format!("Something newer depends on it: undo {} first.", entry.what),
        });
    }
    let out = git::undo(repo, &entry)?;
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut entries = load(repo);
    if let Some(e) = entries.iter_mut().find(|e| e.id == entry.id) {
        e.undone = true;
    }
    save(repo, &entries)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("headway-test-oplog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"]] {
            git::run(&dir, args).unwrap();
        }
        dir
    }

    fn commit(dir: &Path, text: &str) -> GitResult<String> {
        std::fs::write(dir.join("a.txt"), text).unwrap();
        git::stage(dir, &["a.txt".into()])?;
        git::commit(dir, text, false)
    }

    #[test]
    fn entries_undo_newest_first_and_only_once() {
        let dir = temp_repo("order");
        commit(&dir, "1").unwrap();
        let (_, first) = run_recorded(&dir, &Undoable::Refs, "commit “2”", |d| commit(d, "2")).unwrap();
        let (_, second) = run_recorded(&dir, &Undoable::Refs, "commit “3”", |d| commit(d, "3")).unwrap();
        let (first, second) = (first.unwrap(), second.unwrap());
        assert!(second.id > first.id);
        assert_eq!(latest(&dir).unwrap().what, "commit “3”");

        let err = undo(&dir, Some(first.id)).unwrap_err();
        assert!(err.contains("undo commit “3” first"), "{err}");
        assert_eq!(undo(&dir, Some(second.id)).unwrap(), "Undid commit “3”");
        assert_eq!(undo(&dir, Some(second.id)).unwrap_err(), "That has already been undone.");
        assert_eq!(latest(&dir).unwrap().id, first.id);
        undo(&dir, None).unwrap();
        assert_eq!(undo(&dir, None).unwrap_err(), "There's nothing to undo.");
        assert_eq!(git::run(&dir, &["log", "-1", "--format=%s"]).unwrap().trim(), "1");
        let _ = std::fs::remove_file(log_file(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_keeps_the_newest_hundred() {
        let dir = temp_repo("cap");
        commit(&dir, "0").unwrap();
        let base = git::finish_undo(&dir, git::prepare_undo(&dir, &Undoable::Refs).unwrap(), "x").unwrap();
        assert!(base.is_none(), "nothing changed");
        let prep = git::prepare_undo(&dir, &Undoable::Refs).unwrap();
        commit(&dir, "1").unwrap();
        let entry = git::finish_undo(&dir, prep, "commit").unwrap().unwrap();
        for i in 0..KEEP + 5 {
            push(&dir, UndoEntry { what: format!("action {i}"), ..entry.clone() }).unwrap();
        }
        let all = load(&dir);
        assert_eq!(all.len(), KEEP);
        assert_eq!(all[0].what, "action 5");
        assert_eq!(all.last().unwrap().what, format!("action {}", KEEP + 4));
        let _ = std::fs::remove_file(log_file(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_action_leaves_no_entry_and_same_names_get_their_own_logs() {
        let dir = temp_repo("failed");
        commit(&dir, "1").unwrap();
        assert!(run_recorded(&dir, &Undoable::Refs, "nothing", |d| git::run(d, &["checkout", "no-such-branch"])).is_err());
        assert!(latest(&dir).is_none());
        assert_ne!(log_file(Path::new("/a/web")), log_file(Path::new("/b/web")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
