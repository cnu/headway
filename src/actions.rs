//! Every user action in one table: an id, a title, default shortcuts and what must be
//! true for it to be available. Toolbar buttons, menus, shortcuts and the command
//! palette all run these (as `Action::Run(id)`), so they can't disagree about what an
//! action does or when it's allowed.
//!
//! Most actions only need a snapshot of the open repository (`Ctx`) and are built
//! by `build`, which is pure. A few change the window itself (the palette, the
//! sidebar, tabs) and are handled by the app.

use crate::app::{Action, Dialog, FileSel, OpKind, Record, RepoView, Selection};
use crate::git::{self, FileChange};
use crate::views;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Id {
    Palette,
    CheatSheet,
    Fetch,
    Pull,
    Push,
    Stash,
    StashPop,
    NewBranch,
    Checkout,
    Commit,
    StageAll,
    ToggleAmend,
    FocusCommitBox,
    StageFile,
    UnstageFile,
    DiscardFile,
    FixupInto,
    RebaseFrom,
    CopyHash,
    Refresh,
    Undo,
    FindFile,
    ToggleSidebar,
    FocusSearch,
    ToggleSplitDiff,
    AddRepo,
    CloneRepo,
    SwitchRepo,
    NextTab,
    PrevTab,
    CloseTab,
    Overview,
    NextPane,
    PrevPane,
}

/// What has to be true for an action to be available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// A repository is open.
    Repo,
    /// The repository has a remote.
    Remote,
    Upstream,
    /// A branch is checked out (not a detached HEAD).
    Branch,
    Commits,
    Changes,
    Staged,
    /// Something is staged, or the last commit is being amended.
    StagedOrAmend,
    /// The last commit isn't being amended.
    NotAmending,
    Unstaged,
    NoConflicts,
    Stashes,
    /// A commit is selected in the history.
    Commit,
    /// A commit the checked-out one reaches is selected.
    CommitOnHead,
    /// A file is picked in the list of changes.
    FileUnstaged,
    FileStaged,
    /// A commit message is written (or amending).
    Message,
    /// More than one tab is open.
    ManyTabs,
    /// A repository tab is open (not the overview).
    Tab,
    /// The list of uncommitted changes is on screen.
    StagingView,
}

/// Where a shortcut works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Anywhere in the window.
    Anywhere,
    /// Only while the list of uncommitted changes is shown (these are single keys).
    Staging,
    /// Only while the commit message box has focus (the box checks it itself).
    CommitBox,
}

pub struct Def {
    pub id: Id,
    /// The name used in the settings file.
    pub key: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    /// What opens the palette's search for it: more words to find it by.
    pub also: &'static str,
    pub shortcuts: &'static [&'static str],
    pub needs: &'static [Need],
    pub scope: Scope,
    /// The shortcut also works while typing in a text box (true for ones with Ctrl/Cmd or Alt, except these).
    pub not_while_typing: bool,
}

const fn def(id: Id, key: &'static str, title: &'static str, group: &'static str, shortcuts: &'static [&'static str], needs: &'static [Need]) -> Def {
    Def { id, key, title, group, also: "", shortcuts, needs, scope: Scope::Anywhere, not_while_typing: false }
}

const fn also(mut d: Def, words: &'static str) -> Def {
    d.also = words;
    d
}

const fn scoped(mut d: Def, scope: Scope) -> Def {
    d.scope = scope;
    d
}

use Need::*;

pub static DEFS: &[Def] = &[
    also(def(Id::Palette, "palette.open", "Command palette", "General", &["Ctrl+K", "Ctrl+Shift+P"], &[]), "commands actions search"),
    def(Id::CheatSheet, "help.keys", "Keyboard shortcuts", "General", &["?"], &[]),
    def(Id::Refresh, "view.refresh", "Refresh", "General", &["Ctrl+R"], &[]),
    Def { not_while_typing: true, ..def(Id::Undo, "general.undo", "Undo the last action", "General", &["Ctrl+Z"], &[Repo]) },
    also(def(Id::Fetch, "git.fetch", "Fetch", "Remote", &[], &[Repo, Remote]), "download remote"),
    also(def(Id::Pull, "git.pull", "Pull", "Remote", &[], &[Repo, Remote, Upstream]), "download merge update"),
    also(def(Id::Push, "git.push", "Push", "Remote", &[], &[Repo, Remote, Branch]), "upload publish send"),
    also(def(Id::Stash, "git.stash", "Stash changes", "Changes", &[], &[Repo, Changes]), "shelve put aside"),
    also(def(Id::StashPop, "git.stash-pop", "Restore the latest stash", "Changes", &[], &[Repo, Stashes]), "pop apply unstash"),
    also(def(Id::NewBranch, "branch.new", "New branch…", "Branches", &[], &[Repo, Commits]), "create"),
    also(def(Id::Checkout, "branch.checkout", "Switch branch…", "Branches", &[], &[Repo]), "checkout"),
    scoped(def(Id::Commit, "commit.commit", "Commit", "Changes", &["Ctrl+Enter"], &[Repo, StagedOrAmend, NoConflicts, Message]), Scope::CommitBox),
    also(def(Id::StageAll, "stage.all", "Stage all changes", "Changes", &[], &[Repo, Unstaged]), "add"),
    def(Id::ToggleAmend, "commit.amend", "Amend the last commit (toggle)", "Changes", &[], &[Repo, Commits]),
    scoped(def(Id::FocusCommitBox, "commit.focus", "Write the commit message", "Changes", &["C"], &[Repo, StagingView]), Scope::Staging),
    scoped(also(def(Id::StageFile, "stage.file", "Stage the selected file", "Changes", &["S"], &[Repo, FileUnstaged, StagingView]), "add"), Scope::Staging),
    scoped(def(Id::UnstageFile, "stage.unstage-file", "Unstage the selected file", "Changes", &["U"], &[Repo, FileStaged, StagingView]), Scope::Staging),
    scoped(also(def(Id::DiscardFile, "stage.discard-file", "Discard the selected file's changes…", "Changes", &["D"], &[Repo, FileUnstaged, StagingView]), "revert throw away"), Scope::Staging),
    also(def(Id::FixupInto, "commit.fixup-into", "Fixup into an earlier commit…", "Changes", &[], &[Repo, Staged, NoConflicts, NotAmending]), "squash fold"),
    also(def(Id::RebaseFrom, "history.rebase-from", "Interactively rebase from the selected commit…", "History", &[], &[Repo, CommitOnHead]), "reorder squash reword drop"),
    def(Id::CopyHash, "history.copy-hash", "Copy the selected commit's hash", "History", &[], &[Repo, Commit]),
    def(Id::FindFile, "view.find-file", "Find file…", "View", &["Ctrl+P"], &[Repo]),
    def(Id::ToggleSidebar, "view.sidebar", "Show or hide the sidebar", "View", &["Ctrl+B"], &[Repo]),
    def(Id::FocusSearch, "view.search", "Search commits", "View", &["Ctrl+F"], &[Repo]),
    also(def(Id::ToggleSplitDiff, "view.split-diff", "Toggle side-by-side diffs", "View", &[], &[Repo]), "split unified diff"),
    def(Id::AddRepo, "workspace.add-repo", "Add a repository…", "Workspace", &[], &[]),
    def(Id::CloneRepo, "workspace.clone", "Clone from a web address…", "Workspace", &[], &[]),
    also(def(Id::SwitchRepo, "workspace.switch-repo", "Go to a repository…", "Workspace", &[], &[]), "open tab"),
    def(Id::NextTab, "workspace.next-tab", "Next tab", "Workspace", &["Ctrl+PageDown"], &[ManyTabs]),
    def(Id::PrevTab, "workspace.prev-tab", "Previous tab", "Workspace", &["Ctrl+PageUp"], &[ManyTabs]),
    Def { not_while_typing: true, ..def(Id::CloseTab, "workspace.close-tab", "Close the tab", "Workspace", &["Ctrl+W"], &[Tab]) },
    def(Id::Overview, "workspace.overview", "Go to the overview", "Workspace", &[], &[Tab]),
    also(def(Id::NextPane, "view.next-list", "Move the keyboard to the next list", "View", &["F6"], &[Repo]), "focus pane history files branches"),
    def(Id::PrevPane, "view.prev-list", "Move the keyboard to the previous list", "View", &["Shift+F6"], &[Repo]),
];

pub fn def_of(id: Id) -> &'static Def {
    DEFS.iter().find(|d| d.id == id).expect("every action is in the table")
}

pub fn by_key(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// The file picked in the list of changes, with what git says about it.
#[derive(Clone, Debug)]
pub struct FilePick {
    pub sel: FileSel,
    pub change: FileChange,
}

/// What the actions need to know about the window right now.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
    pub upstream: bool,
    pub remote: Option<String>,
    pub commits: bool,
    pub changes: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub conflicts: bool,
    pub stashes: usize,
    pub commit: Option<String>,
    pub commit_on_head: bool,
    pub file: Option<FilePick>,
    pub undo: Option<u64>,
    pub commit_msg: String,
    pub amend: bool,
    /// The list of uncommitted changes is what's on screen.
    pub staging_view: bool,
    pub tabs: usize,
    pub tab: Option<usize>,
}

impl Ctx {
    /// Just the tabs (the overview is showing, or nothing is loaded).
    pub fn tabs_only(tabs: usize, tab: Option<usize>) -> Self {
        Self { tabs, tab, ..Default::default() }
    }

    pub fn of(rv: &RepoView, tabs: usize, tab: Option<usize>) -> Self {
        let mut c = Self::tabs_only(tabs, tab);
        c.repo = Some(rv.path.clone());
        c.commit_msg = rv.commit_msg.clone();
        c.amend = rv.amend;
        let Some(d) = &rv.data else { return c };
        let head = &d.status.head;
        c.branch = head.branch.clone();
        c.upstream = head.upstream.is_some();
        c.remote = d.remotes.first().cloned();
        c.commits = !d.commits.is_empty();
        c.changes = d.status.change_count();
        c.staged = d.status.staged.len();
        c.unstaged = d.status.unstaged.len();
        c.conflicts = !d.status.conflicts.is_empty();
        c.stashes = d.stashes.len();
        c.undo = d.undo.as_ref().map(|e| e.id);
        if let Some(Selection::Commit(h)) = &rv.selection {
            c.commit_on_head = d.on_head.contains(h);
            c.commit = Some(h.clone());
        }
        if let Some(sel) = &rv.file_sel {
            let list = if sel.staged { &d.status.staged } else { &d.status.unstaged };
            c.file = list.iter().find(|f| f.path == sel.path).map(|f| FilePick { sel: sel.clone(), change: f.clone() });
        }
        let showing_other = rv.resolver.as_ref().is_some_and(|r| r.shown)
            || rv.pr_view.as_ref().is_some_and(|p| p.shown)
            || rv.file_view.as_ref().is_some_and(|f| f.shown);
        c.staging_view = !showing_other && matches!(rv.selection, Some(Selection::WorkingDir) | None);
        c
    }
}

fn met(n: Need, c: &Ctx) -> bool {
    match n {
        Repo => c.repo.is_some(),
        Remote => c.remote.is_some(),
        Upstream => c.upstream,
        Branch => c.branch.is_some(),
        Commits => c.commits,
        Changes => c.changes > 0,
        Staged => c.staged > 0,
        StagedOrAmend => c.staged > 0 || (c.amend && c.commits),
        NotAmending => !c.amend,
        Unstaged => c.unstaged > 0,
        NoConflicts => !c.conflicts,
        Stashes => c.stashes > 0,
        Commit => c.commit.is_some(),
        CommitOnHead => c.commit.is_some() && c.commit_on_head,
        FileUnstaged => c.file.as_ref().is_some_and(|f| !f.sel.staged),
        FileStaged => c.file.as_ref().is_some_and(|f| f.sel.staged),
        Message => !c.commit_msg.trim().is_empty(),
        ManyTabs => c.tabs > 1,
        Tab => c.tab.is_some(),
        StagingView => c.staging_view,
    }
}

/// Whether the action can run now.
pub fn available(id: Id, c: &Ctx) -> bool {
    def_of(id).needs.iter().all(|n| met(*n, c))
}

/// What's missing, in a few words, for a tooltip or a greyed-out palette entry.
pub fn missing(id: Id, c: &Ctx) -> Option<&'static str> {
    let n = def_of(id).needs.iter().find(|n| !met(**n, c))?;
    Some(match n {
        Repo | Tab => "Open a repository first",
        Remote => "This repository has no remote",
        Upstream => "This branch has no upstream branch yet",
        Branch => "Switch to a branch first",
        Commits => "There are no commits yet",
        Changes => "There are no changes",
        Staged | StagedOrAmend => "Stage some files first",
        NotAmending => "Not while amending the last commit",
        Unstaged => "There's nothing left to stage",
        NoConflicts => "Resolve the conflicts first",
        Stashes => "There are no stashes",
        Commit => "Select a commit first",
        CommitOnHead => "Select a commit on the checked-out branch first",
        FileUnstaged => "Pick an unstaged file first",
        FileStaged => "Pick a staged file first",
        Message => "Write a message first",
        ManyTabs => "Only one tab is open",
        StagingView => "Show the uncommitted changes first",
    })
}

/// The "Commit" (or "Amend commit") git action.
pub fn commit_action(repo: &Path, msg: &str, amend: bool) -> Action {
    let what = format!("{} “{}”", if amend { "amend" } else { "commit" }, msg.lines().next().unwrap_or("").trim());
    let msg = msg.to_string();
    Action::Git {
        repo: repo.to_path_buf(),
        label: if amend { "Amend commit".into() } else { "Commit".into() },
        op: Box::new(move |p| git::commit(p, &msg, amend)),
        kind: OpKind::Commit,
        record: Some(Record { undo: git::Undoable::Refs, what }),
    }
}

/// Stage everything, then commit it.
pub fn stage_all_and_commit(repo: &Path, msg: &str) -> Action {
    let what = format!("commit “{}”", msg.lines().next().unwrap_or("").trim());
    let msg = msg.to_string();
    Action::Git {
        repo: repo.to_path_buf(),
        label: "Commit".into(),
        op: Box::new(move |p| {
            git::run(p, &["add", "-A"])?;
            git::commit(p, &msg, false)
        }),
        kind: OpKind::Commit,
        record: Some(Record { undo: git::Undoable::Refs, what }),
    }
}

pub fn fetch(repo: &Path) -> Action {
    Action::args(repo, "Fetch", &["fetch", "--all", "--prune"])
}

pub fn pull(repo: &Path) -> Action {
    Action::args(repo, "Pull", &["-c", "pull.rebase=false", "pull", "--no-edit"]).undoable(git::Undoable::Tree { save_work: false }, "pull")
}

pub fn stash(repo: &Path) -> Action {
    Action::args(repo, "Stash changes", &["stash", "push", "--include-untracked"])
}

pub fn stash_pop(repo: &Path) -> Action {
    Action::args(repo, "Restore stash", &["stash", "pop"])
}

/// The "Clone from a web address" dialog.
pub fn clone_dialog() -> Action {
    let dest = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
    Action::Dialog(Dialog::Clone { url: String::new(), dest })
}

/// What an action does, when it only depends on the open repository. Actions that
/// change the window itself return nothing here: the app handles them.
pub fn build(id: Id, c: &Ctx) -> Vec<Action> {
    let Some(p) = c.repo.as_deref() else {
        return match id {
            Id::AddRepo => vec![Action::PickRepo],
            Id::CloneRepo => vec![clone_dialog()],
            Id::Refresh => vec![Action::Refresh],
            Id::NextTab | Id::PrevTab => vec![Action::SelectTab(next_tab(id, c))],
            _ => Vec::new(),
        };
    };
    match id {
        Id::Fetch => vec![fetch(p)],
        Id::Pull => vec![pull(p)],
        Id::Push => vec![views::push_action(p, c.branch.as_deref(), c.upstream, c.remote.as_ref())],
        Id::Stash => vec![stash(p)],
        Id::StashPop => vec![stash_pop(p)],
        Id::NewBranch => {
            let at = c.branch.clone().unwrap_or_else(|| "detached".into());
            vec![views::new_branch(p, "HEAD", &format!("the current branch ({at})"))]
        }
        Id::Commit => vec![commit_action(p, &c.commit_msg, c.amend)],
        Id::StageAll => vec![views::stage_all(p)],
        Id::ToggleAmend => vec![Action::ToggleAmend],
        Id::StageFile => c.file.iter().map(|f| views::file_action(p, "Stage", f.change.clone())).collect(),
        Id::UnstageFile => c.file.iter().map(|f| views::file_action(p, "Unstage", f.change.clone())).collect(),
        Id::DiscardFile => c.file.iter().map(|f| views::file_action(p, "Discard", f.change.clone())).collect(),
        Id::FixupInto => vec![Action::FixupInto],
        Id::RebaseFrom => c.commit.iter().map(|h| Action::RebaseFrom(h.clone())).collect(),
        Id::CopyHash => c.commit.iter().map(|h| Action::Copy(h.clone())).collect(),
        Id::Refresh => vec![Action::Refresh],
        Id::Undo => vec![Action::Undo { repo: p.to_path_buf(), id: c.undo }],
        Id::FindFile => vec![Action::FindFile],
        Id::AddRepo => vec![Action::PickRepo],
        Id::CloneRepo => vec![clone_dialog()],
        Id::NextTab | Id::PrevTab => vec![Action::SelectTab(next_tab(id, c))],
        Id::CloseTab => c.tab.map(Action::CloseTab).into_iter().collect(),
        Id::Overview => vec![Action::SelectTab(None)],
        // The window itself: the app does these.
        Id::Palette
        | Id::CheatSheet
        | Id::Checkout
        | Id::FocusCommitBox
        | Id::ToggleSidebar
        | Id::FocusSearch
        | Id::ToggleSplitDiff
        | Id::SwitchRepo
        | Id::NextPane
        | Id::PrevPane => Vec::new(),
    }
}

fn next_tab(id: Id, c: &Ctx) -> Option<usize> {
    let n = c.tabs.max(1);
    let cur = c.tab;
    Some(match (id, cur) {
        (Id::NextTab, Some(i)) => (i + 1) % n,
        (Id::PrevTab, Some(i)) => (i + n - 1) % n,
        (Id::NextTab, None) => 0,
        _ => n - 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_ctx() -> Ctx {
        Ctx { repo: Some(PathBuf::from("/r")), commits: true, ..Default::default() }
    }

    #[test]
    fn the_table_has_every_action_once_with_unique_keys() {
        let mut keys: Vec<&str> = DEFS.iter().map(|d| d.key).collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "action keys are unique");
        for d in DEFS {
            assert_eq!(def_of(d.id).key, d.key, "{:?} appears once", d.id);
            assert!(!d.title.is_empty());
        }
        assert_eq!(DEFS.len(), 34);
    }

    #[test]
    fn availability_follows_the_repository_state() {
        let mut c = repo_ctx();
        assert!(!available(Id::Fetch, &c), "no remote");
        assert_eq!(missing(Id::Fetch, &c), Some("This repository has no remote"));
        c.remote = Some("origin".into());
        assert!(available(Id::Fetch, &c));
        assert!(!available(Id::Pull, &c), "no upstream");
        assert!(!available(Id::Push, &c), "detached");
        c.branch = Some("main".into());
        assert!(available(Id::Push, &c));
        assert!(!available(Id::Stash, &c) && !available(Id::StageAll, &c));
        c.changes = 2;
        c.unstaged = 2;
        assert!(available(Id::Stash, &c) && available(Id::StageAll, &c));
        assert!(!available(Id::Commit, &c), "nothing staged, no message");
        c.staged = 1;
        assert!(!available(Id::Commit, &c), "no message");
        c.commit_msg = "  ".into();
        assert!(!available(Id::Commit, &c), "blank message");
        c.commit_msg = "Add it".into();
        assert!(available(Id::Commit, &c));
        c.conflicts = true;
        assert!(!available(Id::Commit, &c));
        assert!(!available(Id::Fetch, &Ctx::default()), "no repository");
    }

    #[test]
    fn selection_needs() {
        let mut c = repo_ctx();
        assert!(!available(Id::CopyHash, &c) && !available(Id::RebaseFrom, &c));
        c.commit = Some("abc".into());
        assert!(available(Id::CopyHash, &c));
        assert!(!available(Id::RebaseFrom, &c), "only commits the checked-out one reaches");
        c.commit_on_head = true;
        assert!(available(Id::RebaseFrom, &c));
        c.staging_view = true;
        assert!(!available(Id::StageFile, &c), "no file picked");
        let change = FileChange { path: "a.txt".into(), old_path: None, change: git::Change::Modified, submodule: false };
        c.file = Some(FilePick { sel: FileSel { path: "a.txt".into(), staged: false, untracked: false }, change });
        assert!(available(Id::StageFile, &c) && available(Id::DiscardFile, &c));
        assert!(!available(Id::UnstageFile, &c));
        c.staging_view = false;
        assert!(!available(Id::StageFile, &c), "the single keys only work in the changes list");
    }

    #[test]
    fn building_gives_the_same_actions_the_buttons_do() {
        let mut c = repo_ctx();
        c.remote = Some("origin".into());
        c.branch = Some("main".into());
        assert!(matches!(build(Id::Fetch, &c).as_slice(), [Action::Git { label, .. }] if label == "Fetch"));
        assert!(matches!(build(Id::Push, &c).as_slice(), [Action::Git { label, .. }] if label == "Publish main"), "no upstream yet: publish");
        c.upstream = true;
        assert!(matches!(build(Id::Push, &c).as_slice(), [Action::Git { label, .. }] if label == "Push"));
        assert!(matches!(build(Id::Pull, &c).as_slice(), [Action::Git { record: Some(_), .. }]), "pull can be undone");
        assert!(matches!(build(Id::NewBranch, &c).as_slice(), [Action::Dialog(Dialog::CreateBranch { .. })]));
        assert!(build(Id::Palette, &c).is_empty(), "window actions are the app's");
    }

    #[test]
    fn tabs_wrap_around() {
        let c = |tab| Ctx::tabs_only(3, tab);
        assert!(matches!(build(Id::NextTab, &c(Some(2))).as_slice(), [Action::SelectTab(Some(0))]));
        assert!(matches!(build(Id::PrevTab, &c(Some(0))).as_slice(), [Action::SelectTab(Some(2))]));
        assert!(matches!(build(Id::NextTab, &c(None)).as_slice(), [Action::SelectTab(Some(0))]));
        assert!(matches!(build(Id::PrevTab, &c(None)).as_slice(), [Action::SelectTab(Some(2))]));
        assert!(!available(Id::NextTab, &Ctx::tabs_only(1, Some(0))));
    }
}
