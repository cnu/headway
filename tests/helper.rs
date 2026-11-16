//! The hidden commands git runs in place of an editor during an interactive
//! rebase, tried on the real program.

use std::path::{Path, PathBuf};
use std::process::Command;

fn headway() -> &'static str {
    env!("CARGO_BIN_EXE_headway")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("headway-helper-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn write_rebase_todo_copies_our_file_over_gits() {
    let dir = scratch("copy");
    let (ours, theirs) = (dir.join("ours"), dir.join("theirs"));
    std::fs::write(&ours, "pick abc one\n").unwrap();
    std::fs::write(&theirs, "pick xyz something else\n").unwrap();
    let out = Command::new(headway()).arg("--write-rebase-todo").arg(&ours).arg(&theirs).output().unwrap();
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "pick abc one\n");
    // A missing file is an error, not a window.
    let out = Command::new(headway()).arg("--write-rebase-todo").arg(dir.join("none")).arg(&theirs).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Couldn't write the rebase todo list"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_rebase_message_without_a_session_leaves_the_message_alone() {
    let dir = scratch("message");
    let msg = dir.join("COMMIT_EDITMSG");
    std::fs::write(&msg, "original\n").unwrap();
    let out = Command::new(headway()).arg("--write-rebase-message").arg(dir.join("no-session")).arg(&msg).output().unwrap();
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&msg).unwrap(), "original\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// git starts the editor through `sh`, so a program in a folder with spaces and
/// quotes in its name has to be quoted the way Headway quotes it.
#[cfg(unix)]
#[test]
fn git_runs_it_as_the_sequence_editor_from_an_awkward_folder() {
    let dir = scratch("git");
    let odd = dir.join("it's a folder");
    std::fs::create_dir_all(&odd).unwrap();
    let exe = odd.join("head way");
    std::os::unix::fs::symlink(headway(), &exe).unwrap();
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    for n in ["one", "two", "three"] {
        std::fs::write(repo.join(n), n).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", n]);
    }
    let last = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let todo = dir.join("our todo");
    std::fs::write(&todo, format!("drop {last} three\n")).unwrap();
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let editor = format!("{} --write-rebase-todo {}", quote(&exe), quote(&todo));
    let out = Command::new("git").arg("-C").arg(&repo).args(["rebase", "-i", "HEAD~1"]).env("GIT_SEQUENCE_EDITOR", editor).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(git(&repo, &["log", "--format=%s"]).lines().collect::<Vec<_>>(), vec!["two", "one"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole round trip with the real program: git runs it for the todo list and again
/// for the message of a `reword`, which it takes from the session folder.
#[cfg(unix)]
#[test]
fn git_gets_the_todo_and_a_reworded_message_from_the_program() {
    let dir = scratch("reword");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    for n in ["one", "two"] {
        std::fs::write(repo.join(n), n).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", n]);
    }
    let last = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let session = dir.join("session");
    std::fs::create_dir_all(&session).unwrap();
    let rebase_merge = repo.join(".git").join("rebase-merge");
    let json = format!(
        r#"{{"orig_head":"x","what":"test","rebase_merge":{:?},"messages":{{"rewords":{{"{last}":"Second, reworded\n\nwith a body"}},"squashes":{{}}}}}}"#,
        rebase_merge.to_string_lossy()
    );
    std::fs::write(session.join("session.json"), json).unwrap();
    let todo = session.join("todo");
    std::fs::write(&todo, format!("reword {last} two\n")).unwrap();
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rebase", "-i", "HEAD~1"])
        .env("GIT_SEQUENCE_EDITOR", format!("{} --write-rebase-todo {}", quote(Path::new(headway())), quote(&todo)))
        .env("GIT_EDITOR", format!("{} --write-rebase-message {}", quote(Path::new(headway())), quote(&session)))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(git(&repo, &["log", "-1", "--format=%B"]).trim_end(), "Second, reworded\n\nwith a body");
    let _ = std::fs::remove_dir_all(&dir);
}
