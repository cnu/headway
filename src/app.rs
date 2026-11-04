//! Application state, background work and the outer frame (workspace bar, tabs,
//! dialogs, notifications).

use crate::git::{self, Branch, Change, Commit, CommitDetail, DiffFile, GitResult, Stash, Status};
use crate::graph::{self, Graph};
use crate::theme::{self, *};
use crate::workspace::{Config, Workspace};
use crate::fileview::{self, FileView, Mode};
use crate::resolver::Resolver;
use crate::{diffmodel, highlight, overview, preview, views};
use egui::{Align, Color32, CornerRadius, Id, Layout, Margin, RichText, Sense, Stroke, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

pub type GitFn = Box<dyn FnOnce(&Path) -> GitResult<String> + Send>;

pub const PAGE: usize = 3000;

// ---------------------------------------------------------------------------
// Loaded data

pub struct RepoData {
    pub status: Status,
    pub commits: Vec<Commit>,
    pub graph: Graph,
    pub branches: Vec<Branch>,
    pub tags: Vec<String>,
    pub stashes: Vec<Stash>,
    pub remotes: Vec<String>,
    pub worktrees: Vec<git::Worktree>,
    /// Each remote's address.
    pub remote_urls: Vec<(String, String)>,
    pub submodules: Vec<git::Submodule>,
    /// Loaded commits your branches or tags reach (their messages can be edited).
    pub on_branch: std::collections::HashSet<String>,
    /// Loaded commits that some remote branch already has.
    pub pushed: std::collections::HashSet<String>,
    /// Branches hidden from the history (full ref names).
    pub hidden: Vec<String>,
    pub limit: usize,
    /// A merge, rebase, cherry-pick or revert waiting to be continued or aborted.
    pub operation: Option<git::Operation>,
    /// What Undo would take back.
    pub undo: Option<git::UndoEntry>,
}

fn load_repo(path: &Path, limit: usize, hidden: Vec<String>) -> Result<RepoData, String> {
    if !path.exists() {
        return Err("This folder no longer exists.".into());
    }
    if !git::is_repo(path) {
        return Err("This folder is not a git repository.".into());
    }
    let crashed = |_| "A background task stopped unexpectedly".to_string();
    std::thread::scope(|s| {
        let st = s.spawn(|| git::status(path));
        let lg = s.spawn(|| git::log_filtered(path, limit, &hidden));
        let br = s.spawn(|| git::branches(path));
        let sh = s.spawn(|| git::stashes(path));
        let rm = s.spawn(|| git::remotes(path));
        let wt = s.spawn(|| git::worktrees(path));
        let ru = s.spawn(|| git::remote_urls(path));
        let sm = s.spawn(|| git::submodules(path));
        let op = s.spawn(|| git::operation(path));
        let status = st.join().map_err(crashed)??;
        let commits = lg.join().map_err(crashed)??;
        let (branches, tags) = br.join().map_err(crashed)??;
        let stashes = sh.join().map_err(crashed)?.unwrap_or_default();
        let remotes = rm.join().map_err(crashed)?.unwrap_or_default();
        let worktrees = wt.join().map_err(crashed)?.unwrap_or_default();
        let remote_urls = ru.join().map_err(crashed)?.unwrap_or_default();
        let submodules = sm.join().map_err(crashed)?.unwrap_or_default();
        let operation = op.join().map_err(crashed)?.unwrap_or_default();
        let graph = graph::layout(&commits);
        // Commits your branches or tags reach can have their messages edited.
        let local_tips: Vec<&str> = commits
            .iter()
            .filter(|c| c.refs.iter().any(|r| matches!(r.kind, git::RefKind::Head | git::RefKind::Local | git::RefKind::Tag)))
            .map(|c| c.hash.as_str())
            .chain(status.head.oid.iter().map(|s| s.as_str()))
            .collect();
        let on_branch = reachable(&commits, local_tips.into_iter());
        let remote_tips: Vec<&str> =
            commits.iter().filter(|c| c.refs.iter().any(|r| r.kind == git::RefKind::Remote)).map(|c| c.hash.as_str()).collect();
        let pushed = reachable(&commits, remote_tips.into_iter());
        let undo = crate::oplog::latest(path);
        Ok(RepoData { status, commits, graph, branches, tags, stashes, remotes, worktrees, remote_urls, submodules, on_branch, pushed, hidden: hidden.clone(), limit, operation, undo })
    })
}

/// Commits (among those loaded) reachable from the given starting commits.
fn reachable<'a>(commits: &[Commit], starts: impl Iterator<Item = &'a str>) -> std::collections::HashSet<String> {
    let index: HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.hash.as_str(), i)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<usize> = starts.filter_map(|h| index.get(h).copied()).collect();
    while let Some(i) = stack.pop() {
        if seen.insert(commits[i].hash.clone()) {
            stack.extend(commits[i].parents.iter().filter_map(|p| index.get(p.as_str()).copied()));
        }
    }
    seen
}

pub struct Summary {
    pub status: Status,
    pub last: Option<Commit>,
    pub has_remote: bool,
    /// A linked worktree of another repository (its `.git` is a file).
    pub linked_worktree: bool,
    /// This folder with symlinks resolved, to match it against other repositories.
    pub canonical: PathBuf,
    /// For a linked worktree: its main repository's folder (resolved).
    pub main_repo: Option<PathBuf>,
    /// For a main repository: its other worktrees.
    pub worktrees: Vec<WorktreeInfo>,
    /// Number of submodules, and how many aren't set up yet.
    pub submodules: (usize, usize),
    /// What Undo would take back: (entry id, what was done).
    pub undo: Option<(u64, String)>,
}

/// A worktree shown inside its main repository's card.
#[derive(Clone, Debug)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    /// The folder is gone.
    pub prunable: bool,
    pub locked: bool,
    /// Uncommitted changes, ahead, behind (when it could be read).
    pub status: Option<(usize, u32, u32)>,
}

fn load_summary(path: &Path) -> Result<Summary, String> {
    if !git::is_repo(path) {
        return Err("Not a git repository (moved or deleted?)".into());
    }
    let status = git::status(path)?;
    let last = git::run(path, &["log", "-1", "--format=%H%x1f%an%x1f%at%x1f%s"]).ok().and_then(|o| {
        let f: Vec<&str> = o.trim_end().split('\x1f').collect();
        (f.len() == 4).then(|| Commit {
            hash: f[0].into(),
            parents: vec![],
            author: f[1].into(),
            email: String::new(),
            time: f[2].parse().unwrap_or(0),
            summary: f[3].into(),
            refs: vec![],
        })
    });
    let has_remote = git::remotes(path).map(|r| !r.is_empty()).unwrap_or(false);
    let linked_worktree = path.join(".git").is_file();
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let all = git::worktrees(path).unwrap_or_default();
    let main_repo = if linked_worktree { all.iter().find(|w| w.is_main).map(|w| canon(&w.path)) } else { None };
    let worktrees = if linked_worktree {
        Vec::new()
    } else {
        // Read each worktree's state at the same time.
        std::thread::scope(|s| {
            let jobs: Vec<_> = all
                .iter()
                .filter(|w| !w.is_main && !w.bare)
                .map(|w| {
                    let job = (!w.prunable).then(|| s.spawn(|| git::status(&w.path)));
                    (w, job)
                })
                .collect();
            jobs.into_iter()
                .map(|(w, job)| WorktreeInfo {
                    path: w.path.clone(),
                    branch: w.branch.clone(),
                    head: w.head.clone(),
                    prunable: w.prunable,
                    locked: w.locked,
                    status: job
                        .and_then(|j| j.join().ok())
                        .and_then(|r| r.ok())
                        .map(|st| (st.change_count(), st.head.ahead, st.head.behind)),
                })
                .collect()
        })
    };
    let subs = git::submodules(path).unwrap_or_default();
    let submodules = (subs.len(), subs.iter().filter(|s| s.state == git::SubState::NotSetUp).count());
    let undo = crate::oplog::latest(path).map(|e| (e.id, e.what));
    Ok(Summary { status, last, has_remote, linked_worktree, canonical: canon(path), main_repo, worktrees, submodules, undo })
}

// ---------------------------------------------------------------------------
// Diff documents: a diff flattened into equal-height rows for fast scrolling.

#[derive(Clone, Copy)]
pub enum FlatRow {
    /// A group title ("Unstaged", "Staged") in the stacked uncommitted view.
    Section(usize),
    File(usize),
    Hunk(usize, usize),
    Line(usize, usize, usize),
    Note(usize),
    /// One strip of a changed picture (file, strip): strip 0 holds the titles.
    Image(usize, usize),
    Gap,
    /// A side-by-side row of a hunk (file, hunk, line shown left, line shown right).
    Pair(usize, usize, Option<usize>, Option<usize>),
}

pub struct DiffDoc {
    pub files: Vec<DiffFile>,
    /// The rows currently shown (folded files show only their name).
    pub rows: Vec<FlatRow>,
    /// Row of each file's start (its name bar, when there is one).
    pub file_rows: Vec<usize>,
    pub max_cols: usize,
    with_headers: bool,
    /// Files whose changes are folded away.
    pub collapsed: std::collections::BTreeSet<usize>,
    /// What each file is (a commit's change, or unstaged/staged/new) — decides its buttons.
    pub modes: Vec<views::DiffMode>,
    /// Which files are submodules (no line or block staging for those).
    pub submodule: Vec<bool>,
    /// Group titles: (first file of the group, title).
    pub sections: Vec<(usize, String)>,
    /// Pictures shown for image files (one entry per file).
    pub previews: Vec<Option<preview::ImagePreview>>,
    /// The commit these are the changes of (`None`: uncommitted changes).
    pub commit: Option<String>,
    /// Syntax colours of each file, filled in the background after the diff is shown.
    pub syntax: Arc<Vec<std::sync::OnceLock<highlight::Sides>>>,
    /// The words that changed in lines facing each other: (file, hunk, line) →
    /// byte ranges, and whether only whitespace changed.
    pub marks: HashMap<LineKey, diffmodel::LineMarks>,
    /// Whitespace changes are left out, so the hunks don't match the files and
    /// lines and hunks can't be staged from them.
    pub whitespace_ignored: bool,
    /// Shown side by side (before on the left, after on the right).
    pub split: bool,
}

/// Files with more changed lines than this start folded in the uncommitted view.
pub const LARGE_FILE_LINES: u32 = 300;

impl DiffDoc {
    pub fn new(files: Vec<DiffFile>, with_file_headers: bool) -> Self {
        let max_cols = files
            .iter()
            .flat_map(|f| f.hunks.iter())
            .flat_map(|h| h.lines.iter())
            .map(|l| (l.text.len() + l.text.matches('\t').count() * 3).min(views::MAX_LINE_CHARS + 24))
            .fold(40, usize::max);
        let n = files.len();
        let mut doc = Self {
            files,
            rows: Vec::new(),
            file_rows: Vec::new(),
            max_cols,
            with_headers: with_file_headers,
            collapsed: Default::default(),
            modes: vec![views::DiffMode::Commit; n],
            submodule: vec![false; n],
            sections: Vec::new(),
            previews: Vec::new(),
            commit: None,
            syntax: Arc::new((0..n).map(|_| Default::default()).collect()),
            marks: HashMap::new(),
            whitespace_ignored: false,
            split: false,
        };
        for (fi, f) in doc.files.iter().enumerate() {
            for (hi, h) in f.hunks.iter().enumerate() {
                for (li, marks) in diffmodel::hunk_marks(&h.lines) {
                    doc.marks.insert((fi, hi, li), marks);
                }
            }
        }
        doc.rebuild();
        doc
    }

    /// Read pictures of the image files (slow: call it in the background).
    /// `commit` is the commit shown, when this is a commit's changes.
    pub fn load_previews(&mut self, repo: &Path, commit: Option<&str>) {
        self.previews = preview::load_all(repo, &self.files, &self.modes, commit);
        self.commit = commit.map(String::from);
        self.rebuild();
    }

    /// The work of colouring this diff's code, to run after it's on screen.
    pub fn highlighter(&self, repo: &Path) -> highlight::Job {
        let files = self
            .files
            .iter()
            .enumerate()
            .filter(|(fi, f)| !f.binary && !f.hunks.is_empty() && self.preview(*fi).is_none() && highlight::knows(&f.path))
            .map(|(fi, f)| {
                let new_on_disk = matches!(self.modes.get(fi), Some(views::DiffMode::Unstaged | views::DiffMode::Untracked));
                let old_path = f.old_path.clone().unwrap_or_else(|| f.path.clone());
                (fi, highlight::JobFile { path: f.path.clone(), old_path, old_id: f.old_id.clone(), new_id: f.new_id.clone(), new_on_disk })
            })
            .collect();
        highlight::Job { repo: repo.to_path_buf(), slots: self.syntax.clone(), files }
    }

    /// Syntax colours of one line of file `fi`, once they're ready.
    pub fn spans(&self, fi: usize, l: &git::DiffLine) -> Option<&[highlight::Span]> {
        let s = self.syntax.get(fi)?.get()?;
        let old = || s.old.as_ref()?.line(l.old_no?, &l.text);
        let new = || s.new.as_ref()?.line(l.new_no?, &l.text);
        match l.kind {
            git::LineKind::Del => old(),
            git::LineKind::Add => new(),
            git::LineKind::Context => new().or_else(old),
            git::LineKind::NoNewline => None,
        }
    }

    pub fn preview(&self, fi: usize) -> Option<&preview::ImagePreview> {
        self.previews.get(fi).and_then(|p| p.as_ref())
    }

    /// The stacked uncommitted view: groups of files, each with a title and a kind.
    /// Very large files start folded.
    pub fn working(groups: Vec<(String, views::DiffMode, Vec<(DiffFile, bool)>)>) -> Self {
        let mut files = Vec::new();
        let mut modes = Vec::new();
        let mut submodule = Vec::new();
        let mut sections = Vec::new();
        for (title, mode, list) in groups {
            if list.is_empty() {
                continue;
            }
            sections.push((files.len(), title));
            for (f, sub) in list {
                // A new (untracked) file stages differently from an unstaged edit.
                let m = if mode == views::DiffMode::Unstaged && f.change == Change::Untracked { views::DiffMode::Untracked } else { mode };
                modes.push(m);
                submodule.push(sub);
                files.push(f);
            }
        }
        let mut doc = Self::new(files, true);
        doc.modes = modes;
        doc.submodule = submodule;
        doc.sections = sections;
        doc.collapsed = doc.files.iter().enumerate().filter(|(_, f)| f.added + f.removed > LARGE_FILE_LINES).map(|(i, _)| i).collect();
        doc.rebuild();
        doc
    }

    /// Identifies a file across reloads, to remember whether it was folded.
    pub fn fold_key(&self, fi: usize) -> (String, u8) {
        (self.files[fi].path.clone(), self.modes[fi] as u8)
    }

    /// Whether lines and blocks of this file can be staged/unstaged here.
    pub fn editable(&self, fi: usize) -> bool {
        self.modes.get(fi).is_some_and(|m| *m != views::DiffMode::Commit) && !self.submodule.get(fi).copied().unwrap_or(false) && !self.whitespace_ignored
    }

    /// The changed words of one line, and whether only whitespace changed.
    pub fn marks(&self, fi: usize, hi: usize, li: usize) -> Option<(&[std::ops::Range<usize>], bool)> {
        self.marks.get(&(fi, hi, li)).map(|(r, ws)| (r.as_slice(), *ws))
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        self.file_rows.clear();
        for (fi, f) in self.files.iter().enumerate() {
            if let Some(si) = self.sections.iter().position(|(first, _)| *first == fi) {
                self.rows.push(FlatRow::Section(si));
            }
            self.file_rows.push(self.rows.len());
            if self.with_headers {
                self.rows.push(FlatRow::File(fi));
            }
            // Only files with a name bar can be folded (otherwise nothing would be left to click).
            if !(self.with_headers && self.collapsed.contains(&fi)) {
                if let Some(p) = self.previews.get(fi).and_then(|p| p.as_ref()) {
                    self.rows.extend((0..p.rows()).map(|k| FlatRow::Image(fi, k)));
                } else if f.binary || f.hunks.is_empty() {
                    self.rows.push(FlatRow::Note(fi));
                }
                for (hi, h) in f.hunks.iter().enumerate() {
                    self.rows.push(FlatRow::Hunk(fi, hi));
                    if self.split {
                        self.rows.extend(diffmodel::align(&h.lines).into_iter().map(|a| FlatRow::Pair(fi, hi, a.old, a.new)));
                    } else {
                        self.rows.extend((0..h.lines.len()).map(|li| FlatRow::Line(fi, hi, li)));
                    }
                }
            }
            if self.with_headers {
                self.rows.push(FlatRow::Gap);
            }
        }
    }

    /// Show the changes side by side, or unified.
    pub fn set_split(&mut self, split: bool) {
        if self.split != split {
            self.split = split;
            self.rebuild();
        }
    }

    /// A line of the diff.
    pub fn line(&self, (fi, hi, li): LineKey) -> Option<&git::DiffLine> {
        self.files.get(fi)?.hunks.get(hi)?.lines.get(li)
    }

    pub fn is_collapsed(&self, fi: usize) -> bool {
        self.collapsed.contains(&fi)
    }

    /// Fold or unfold one file's changes.
    pub fn toggle(&mut self, fi: usize) {
        if !self.collapsed.remove(&fi) && fi < self.files.len() {
            self.collapsed.insert(fi);
        }
        self.rebuild();
    }

    pub fn set_collapsed(&mut self, fi: usize, collapsed: bool) {
        if self.is_collapsed(fi) != collapsed {
            self.toggle(fi);
        }
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        self.collapsed = if collapsed { (0..self.files.len()).collect() } else { Default::default() };
        self.rebuild();
    }
}

// ---------------------------------------------------------------------------
// One open repository tab

#[derive(Clone, PartialEq)]
pub enum Selection {
    WorkingDir,
    Commit(String),
    /// The reflog of `RepoView::reflog_ref`.
    Reflog,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSel {
    pub path: String,
    pub staged: bool,
    pub untracked: bool,
}

/// One line of a diff: (file, hunk, line within the hunk).
pub type LineKey = (usize, usize, usize);

/// Lines picked in a diff. Kept by line rather than by row, so a pick means the
/// same lines whether the diff is shown unified or side by side.
#[derive(Default)]
pub struct LineSel {
    pub lines: std::collections::BTreeSet<LineKey>,
    pub anchor: Option<LineKey>,
    pub dragging: bool,
}

pub struct Detail {
    pub info: CommitDetail,
    pub doc: DiffDoc,
}

pub struct RepoView {
    pub path: PathBuf,
    pub data: Option<RepoData>,
    pub error: Option<String>,
    pub loading: bool,
    pub limit: usize,
    pub selection: Option<Selection>,
    pub detail: Option<Detail>,
    pub detail_loading: bool,
    pub file_sel: Option<FileSel>,
    /// Every uncommitted change, stacked.
    pub wd_doc: Option<DiffDoc>,
    /// Files folded or unfolded by hand in the uncommitted view (kept across reloads).
    pub wd_folds: HashMap<(String, u8), bool>,
    wd_gen: u64,
    pub commit_msg: String,
    pub amend: bool,
    pub search: String,
    pub filtered: Vec<usize>,
    filter_key: Option<(String, usize)>,
    pub diff_scroll_to: Option<usize>,
    pub line_sel: LineSel,
    /// A different commit's details just arrived: start its panes scrolled to the top.
    pub reset_detail_scroll: bool,
    pub list_scroll_to: Option<usize>,
    /// History list scroll position and visible height, as of the last frame.
    pub list_view: (f32, f32),
    /// The conflict resolver, when open (it replaces history and details).
    pub resolver: Option<Resolver>,
    conflict_gen: u64,
    /// Whose reflog is shown: "HEAD" or a full branch name.
    pub reflog_ref: String,
    pub reflog: Option<Result<Vec<git::ReflogEntry>, String>>,
    reflog_gen: u64,
    /// Blame or history of one file, folder or set of lines, when open.
    pub file_view: Option<FileView>,
    file_gen: u64,
    /// Lines picked in a commit's diff (for "History of these lines").
    pub detail_sel: LineSel,
    /// A dialog is open over the window (keys belong to it).
    pub modal: bool,
}

impl RepoView {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            data: None,
            error: None,
            loading: false,
            limit: PAGE,
            selection: None,
            detail: None,
            detail_loading: false,
            file_sel: None,
            wd_doc: None,
            wd_folds: HashMap::new(),
            wd_gen: 0,
            commit_msg: String::new(),
            amend: false,
            search: String::new(),
            filtered: Vec::new(),
            filter_key: None,
            diff_scroll_to: None,
            line_sel: LineSel::default(),
            reset_detail_scroll: false,
            list_scroll_to: None,
            list_view: (0.0, 0.0),
            resolver: None,
            conflict_gen: 0,
            reflog_ref: "HEAD".into(),
            reflog: None,
            reflog_gen: 0,
            file_view: None,
            file_gen: 0,
            detail_sel: LineSel::default(),
            modal: false,
        }
    }

    /// Recompute which commits match the search box (cached).
    pub fn refilter(&mut self) {
        let Some(data) = &self.data else { return };
        let key = (self.search.trim().to_lowercase(), data.commits.len());
        if self.filter_key.as_ref() == Some(&key) {
            return;
        }
        let q = &key.0;
        self.filtered = if q.is_empty() {
            (0..data.commits.len()).collect()
        } else {
            data.commits
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.summary.to_lowercase().contains(q)
                        || c.author.to_lowercase().contains(q)
                        || c.hash.starts_with(q.as_str())
                        || c.refs.iter().any(|r| r.name.to_lowercase().contains(q))
                })
                .map(|(i, _)| i)
                .collect()
        };
        self.filter_key = Some(key);
    }

    pub fn head_branch(&self) -> Option<&str> {
        self.data.as_ref()?.status.head.branch.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Messages from worker threads and actions from the UI

enum Msg {
    /// More of a diff's syntax colours are ready (they're in the diff already).
    Highlighted,
    Repo(PathBuf, Result<RepoData, String>),
    Summary(PathBuf, Result<Summary, String>),
    Detail(PathBuf, String, Result<Detail, String>),
    WorkDiff(PathBuf, u64, Result<DiffDoc, String>),
    /// Conflicted files read for the resolver.
    Conflicts(PathBuf, u64, Vec<(String, Result<git::ConflictFile, String>, Option<preview::ConflictPictures>)>),
    Reflog(PathBuf, u64, Result<Vec<git::ReflogEntry>, String>),
    /// The text of a file being blamed, and the repository's list of commits to skip.
    BlameText(PathBuf, u64, Result<Vec<String>, String>, Option<git::IgnoreRevs>),
    /// Syntax colours of the blamed file.
    BlameSyntax(PathBuf, u64, Option<Arc<highlight::Highlighted>>),
    /// More blame results: commits seen for the first time, and the lines they changed.
    BlameChunk(PathBuf, u64, Vec<git::BlameCommit>, Vec<git::BlameRange>),
    BlameDone(PathBuf, u64, Result<(), String>),
    FileLog(PathBuf, u64, Result<Vec<git::HistoryEntry>, String>),
    /// What the commit picked in a file's history changed in that file.
    FileDiff(PathBuf, u64, Result<DiffDoc, String>),
    /// Every tracked file, for "Find file".
    TrackedFiles(PathBuf, Result<Vec<(String, bool)>, String>),
    SamplesReady,
    /// A folder was chosen to add; carries the repositories found in it.
    Picked(PathBuf, Vec<PathBuf>),
    PickClosed,
    /// A folder chosen with "Browse…" in the clone or new-worktree dialog.
    PickedDest(PathBuf),
    LastMessage(PathBuf, String),
    /// A commit's full message, read to open the "Edit commit message" dialog.
    EditMessage { repo: PathBuf, hash: String, message: Result<String, String> },
    /// `undo`: the operation log entry the action left, if it can be undone.
    OpDone { id: u64, repo: PathBuf, label: String, result: Result<String, String>, kind: OpKind, undo: Option<u64> },
    Cloned(u64, Result<PathBuf, String>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum OpKind {
    Normal,
    Commit,
    Quiet,
    /// A commit message was edited; the output is the commit's new id, to select it.
    Reword,
    /// An undo; the output says what was undone.
    Undo,
}

/// Save what a git action changes, so it can be undone.
pub struct Record {
    pub undo: git::Undoable,
    /// What the action does, to follow "Undo: " ("discard hunk in src/app.rs").
    pub what: String,
}

pub enum Action {
    SelectTab(Option<usize>),
    OpenRepo(PathBuf),
    CloseTab(usize),
    SwitchWorkspace(usize),
    RemoveRepo(PathBuf),
    Dialog(Dialog),
    Refresh,
    RefreshOverview,
    SelectCommit(String),
    SelectRef(String),
    SelectWorkingDir,
    /// Show the reflog, of HEAD or a full branch name (None: the one shown last).
    ShowReflog(Option<String>),
    SelectFile(FileSel),
    /// Blame or history of a file, folder or lines, and moving around in it.
    File(FileOpen),
    CloseFile,
    /// Put the file view aside and show this commit in the history.
    ShowCommit(String),
    /// Put the file view aside and show the uncommitted changes.
    ShowWorkingDir,
    /// The "Find file" dialog (Ctrl/Cmd+P).
    FindFile,
    /// Open a conflicted file (by path) in the resolver.
    OpenConflict(String),
    CloseResolver,
    Git { repo: PathBuf, label: String, op: GitFn, kind: OpKind, record: Option<Record> },
    /// A git action with something to do in the app once it succeeds.
    GitThen { repo: PathBuf, label: String, op: GitFn, then: FollowUp },
    /// Add a worktree folder to the workspace (if needed) and open it.
    OpenWorktree(PathBuf),
    /// Show or hide a branch (full ref name) in the open repository's history.
    ToggleHidden(String),
    /// Show every hidden branch of the open repository again.
    ShowAllBranches,
    /// Undo the newest entry in a repository's operation log (only if it's still entry `id`).
    Undo { repo: PathBuf, id: Option<u64> },
    /// Open the "Edit commit message" dialog for this commit of the open repository.
    EditMessage(String),
    LoadMore,
    PickRepo,
    PickFolder,
    Copy(String),
    ToggleAmend,
    FetchAll,
    PullAll,
    /// How diffs are shown changed: show the open ones that way, reading them
    /// again (`reload`) when what they contain changes.
    DiffPrefsChanged { reload: bool },
}

impl Action {
    pub fn git(repo: &Path, label: impl Into<String>, op: impl FnOnce(&Path) -> GitResult<String> + Send + 'static) -> Self {
        Action::Git { repo: repo.to_path_buf(), label: label.into(), op: Box::new(op), kind: OpKind::Normal, record: None }
    }

    /// Save what this git action changes first, so it can be undone. `what` follows
    /// "Undo: " in menus.
    pub fn undoable(self, undo: git::Undoable, what: impl Into<String>) -> Self {
        match self {
            Action::Git { repo, label, op, kind, .. } => Action::Git { repo, label, op, kind, record: Some(Record { undo, what: what.into() }) },
            other => other,
        }
    }

    /// A git action that just runs these arguments.
    pub fn args(repo: &Path, label: impl Into<String>, args: &[&str]) -> Self {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        Self::git(repo, label, move |p| {
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            git::run(p, &a)
        })
    }

    pub fn confirm(title: &str, body: String, button: &str, danger: bool, then: Action) -> Self {
        Action::Dialog(Dialog::Confirm {
            title: title.into(),
            body,
            button: button.into(),
            danger,
            then: Some(Box::new(then)),
        })
    }
}

/// What to show in the file view (blame and history).
#[derive(Clone, Debug, PartialEq)]
pub enum FileOpen {
    /// Blame `path` at `rev` (the working copy when `None`), scrolled to `line`.
    /// `back`: remember the blame shown now, for the Back button.
    Blame { rev: Option<String>, path: String, line: Option<u32>, back: bool },
    /// Commits that changed a file (following renames) or a folder.
    History { path: String, folder: bool },
    /// Commits that changed lines `start..=end` of `path` at `rev`.
    Lines { rev: Option<String>, path: String, start: u32, end: u32 },
    /// Switch between blame and history of what's shown.
    Mode(Mode),
    /// Blame again (the options changed).
    Reload,
    /// The previous blame.
    Back,
    /// Pick a commit in the history.
    SelectCommit(usize),
    /// Bring the view back after looking at a commit.
    Show,
}

/// What to do after a git action succeeds.
pub enum FollowUp {
    /// Add this folder to the workspace and open it.
    Open(PathBuf),
    /// This folder was deleted: remove it from workspaces and tabs.
    Forget(PathBuf),
}

pub enum Dialog {
    NewWorkspace { name: String },
    RenameWorkspace { name: String },
    DeleteWorkspace,
    AddPath { path: String },
    Clone { url: String, dest: String },
    CreateBranch { repo: PathBuf, at: String, label: String, name: String, checkout: bool },
    CreateTag { repo: PathBuf, at: String, label: String, name: String },
    AddRemote { repo: PathBuf, name: String, url: String, fetch: bool, taken: Vec<String> },
    EditMessage { repo: PathBuf, hash: String, message: String, original: String, pushed: bool, is_head: bool },
    AddSubmodule { repo: PathBuf, url: String, path: String, path_edited: bool },
    CreateWorktree {
        repo: PathBuf,
        /// Local branches not checked out anywhere yet.
        free_branches: Vec<String>,
        new_branch: bool,
        existing: String,
        name: String,
        start: String,
        start_label: String,
        path: String,
        path_edited: bool,
    },
    Confirm { title: String, body: String, button: String, danger: bool, then: Option<Box<Action>> },
    /// Find a file or folder by name, to blame it or see its history.
    FindFile {
        repo: PathBuf,
        query: String,
        /// Every tracked file and folder (true for folders), once read.
        files: Option<Result<Vec<(String, bool)>, String>>,
        /// What matches `hits_for`.
        hits: Vec<(String, bool)>,
        hits_for: Option<String>,
        picked: usize,
    },
}

struct Toast {
    text: String,
    error: bool,
    until: f64,
    /// Offers Undo for this repository's operation log entry.
    undo: Option<(PathBuf, u64)>,
}

struct Shot {
    path: PathBuf,
    requested: bool,
    done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

// ---------------------------------------------------------------------------

pub struct HeadwayApp {
    pub cfg: Config,
    tabs: HashMap<PathBuf, RepoView>,
    pub summaries: HashMap<PathBuf, Result<Summary, String>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    dialog: Option<Dialog>,
    toasts: Vec<Toast>,
    pub busy: Vec<(u64, PathBuf, String)>,
    next_id: u64,
    frame_no: u64,
    focused: bool,
    shot: Option<Shot>,
    pending_name: Option<String>,
    /// Background jobs whose answer hasn't been handled yet.
    pending: Arc<std::sync::atomic::AtomicUsize>,
    pending_add: Option<PathBuf>,
    /// The sample repositories are still being created in the background.
    samples_pending: bool,
    /// A folder picker is open (only one at a time).
    picking: bool,
    /// Follow-ups waiting for git actions to finish, by action id.
    followups: HashMap<u64, FollowUp>,
    /// Layout preferences changed and still need saving (waits for the mouse to be released).
    view_dirty: bool,
}

impl HeadwayApp {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>) -> Self {
        let (cfg, needs_samples) = Config::load_or_init();
        Self::with_config(cc.egui_ctx.clone(), cfg, needs_samples, screenshot)
    }

    /// Start with the given settings (tests use their own, saved elsewhere).
    pub fn with_config(ctx: egui::Context, cfg: Config, needs_samples: bool, screenshot: Option<PathBuf>) -> Self {
        theme::apply(&ctx);
        let (tx, rx) = channel();
        let mut app = Self {
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            cfg,
            tabs: HashMap::new(),
            summaries: HashMap::new(),
            tx,
            rx,
            ctx,
            dialog: None,
            toasts: Vec::new(),
            busy: Vec::new(),
            next_id: 1,
            frame_no: 0,
            focused: true,
            pending_name: None,
            pending_add: None,
            samples_pending: needs_samples,
            picking: false,
            followups: HashMap::new(),
            view_dirty: false,
            shot: screenshot.map(|path| Shot {
                path,
                requested: false,
                done: Arc::new(AtomicBool::new(false)),
                error: Arc::new(Mutex::new(None)),
            }),
        };
        if needs_samples {
            // Building the samples runs git many times; keep the window responsive meanwhile.
            app.spawn(|| {
                crate::samples::build_samples();
                Msg::SamplesReady
            });
        } else {
            app.start();
        }
        app
    }

    fn start(&mut self) {
        self.refresh_overview();
        if let Some(p) = self.active_path() {
            self.ensure_tab(&p);
        }
    }

    /// Open the system folder picker without blocking the window.
    fn pick_folder(&mut self, title: &str, on_pick: impl FnOnce(PathBuf) -> Msg + Send + 'static) {
        if self.picking {
            return;
        }
        self.picking = true;
        // Created here (some systems need dialogs started from the main thread), awaited on a worker.
        let fut = rfd::AsyncFileDialog::new().set_title(title).pick_folder();
        self.spawn(move || match pollster::block_on(fut) {
            Some(h) => on_pick(h.path().to_path_buf()),
            None => Msg::PickClosed,
        });
    }

    /// Look for repositories in a folder on a worker, then add them.
    fn scan_and_add(&self, dir: PathBuf) {
        self.spawn(move || {
            let found = git::scan_for_repos(&dir);
            Msg::Picked(dir, found)
        });
    }

    fn spawn(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.pending.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    pub fn active_path(&self) -> Option<PathBuf> {
        let ws = self.cfg.ws();
        ws.active_tab.and_then(|i| ws.open_tabs.get(i).cloned())
    }

    fn ensure_tab(&mut self, path: &Path) {
        if !self.tabs.contains_key(path) {
            self.tabs.insert(path.to_path_buf(), RepoView::new(path.to_path_buf()));
            self.reload_repo(path);
        }
    }

    fn reload_repo(&mut self, path: &Path) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        rv.loading = true;
        let limit = rv.limit;
        let p = path.to_path_buf();
        let hidden = self.cfg.hidden_refs.get(path).cloned().unwrap_or_default();
        self.spawn(move || Msg::Repo(p.clone(), load_repo(&p, limit, hidden)));
    }

    fn reload_summary(&self, path: &Path) {
        let p = path.to_path_buf();
        self.spawn(move || Msg::Summary(p.clone(), load_summary(&p)));
    }

    fn refresh_overview(&self) {
        for r in &self.cfg.ws().repos {
            self.reload_summary(r);
        }
    }

    fn load_detail(&mut self, path: &Path, hash: String) {
        if let Some(rv) = self.tabs.get_mut(path) {
            rv.detail_loading = true;
        }
        let p = path.to_path_buf();
        let (ws, split) = (self.cfg.view.diff.ignore_whitespace, self.cfg.view.diff.split);
        self.spawn_stream(move |send| {
            // Parsing and flattening a big diff can take a while: do it here, not in the UI.
            let d = git::commit_detail(&p, &hash, ws).map(|mut info| {
                let files = std::mem::take(&mut info.files);
                let mut doc = DiffDoc::new(files, true);
                doc.whitespace_ignored = ws;
                doc.set_split(split);
                doc.load_previews(&p, Some(&info.hash));
                Detail { info, doc }
            });
            let job = d.as_ref().ok().map(|d| d.doc.highlighter(&p));
            send(Msg::Detail(p, hash, d));
            if let Some(job) = job {
                job.run(&|| send(Msg::Highlighted));
            }
            Msg::Highlighted
        });
    }

    /// Read every uncommitted change for the stacked view (in the background).
    fn load_work_diff(&mut self, path: &Path) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        let Some(st) = rv.data.as_ref().map(|d| d.status.clone()) else { return };
        rv.wd_gen += 1;
        let generation = rv.wd_gen;
        let p = path.to_path_buf();
        let (ws, split) = (self.cfg.view.diff.ignore_whitespace, self.cfg.view.diff.split);
        self.spawn_stream(move |send| {
            let doc = git::worktree_diffs(&p, &st, ws).map(|(unstaged, staged)| {
                let with_sub = |files: Vec<DiffFile>, list: &[git::FileChange]| {
                    files
                        .into_iter()
                        .map(|f| {
                            let sub = list.iter().any(|c| c.path == f.path && c.submodule);
                            (f, sub)
                        })
                        .collect::<Vec<_>>()
                };
                let mut doc = DiffDoc::working(vec![
                    ("UNSTAGED".to_string(), views::DiffMode::Unstaged, with_sub(unstaged, &st.unstaged)),
                    ("STAGED".to_string(), views::DiffMode::Staged, with_sub(staged, &st.staged)),
                ]);
                doc.whitespace_ignored = ws;
                doc.set_split(split);
                doc.load_previews(&p, None);
                doc
            });
            let job = doc.as_ref().ok().map(|d| d.highlighter(&p));
            send(Msg::WorkDiff(p, generation, doc));
            if let Some(job) = job {
                job.run(&|| send(Msg::Highlighted));
            }
            Msg::Highlighted
        });
    }

    /// Read the shown reflog in the background.
    fn load_reflog(&mut self, path: &Path) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        rv.reflog_gen += 1;
        let generation = rv.reflog_gen;
        let (p, r) = (path.to_path_buf(), rv.reflog_ref.clone());
        self.spawn(move || Msg::Reflog(p.clone(), generation, git::reflog(&p, &r, 1000)));
    }

    /// Read conflicted files for the resolver in the background.
    fn load_conflicts(&mut self, path: &Path, files: Vec<(String, git::ConflictKind)>) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        if files.is_empty() {
            return;
        }
        rv.conflict_gen += 1;
        let generation = rv.conflict_gen;
        let p = path.to_path_buf();
        self.spawn(move || {
            let read = files.into_iter().map(|(f, kind)| {
                let r = git::conflict_file(&p, &f, kind);
                let pictures = preview::load_conflict(&p, &f);
                (f, r, pictures)
            });
            Msg::Conflicts(p.clone(), generation, read.collect())
        });
    }

    /// Like [`Self::spawn`], for work that reports as it goes: `send` passes messages on
    /// while it runs, and the value it returns is the last one.
    fn spawn_stream(&self, f: impl FnOnce(&dyn Fn(Msg)) -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let pending = self.pending.clone();
        self.pending.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let send = |m: Msg| {
                pending.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let last = f(&send);
            let _ = tx.send(last);
            ctx.request_repaint();
        });
    }

    /// Blame the file the file view shows, streaming results in as git finds them.
    fn load_blame(&mut self, path: &Path) {
        let opts = self.cfg.view.blame;
        let Some(rv) = self.tabs.get_mut(path) else { return };
        rv.file_gen += 1;
        let generation = rv.file_gen;
        let Some(b) = rv.file_view.as_mut().and_then(|f| f.blame.as_mut()) else { return };
        b.generation = generation;
        let (p, rev, file) = (path.to_path_buf(), b.rev.clone(), b.path.clone());
        if highlight::knows(&file) {
            let (p, rev, file) = (p.clone(), rev.clone(), file.clone());
            self.spawn(move || {
                let syntax = git::blob_id(&p, rev.as_deref(), &file).ok().and_then(|id| {
                    highlight::cached(&id, &file, || match rev {
                        Some(_) => git::small_blobs(&p, std::slice::from_ref(&id), highlight::MAX_BYTES as u64).ok()?.remove(&id),
                        None => {
                            let full = p.join(&file);
                            let small = std::fs::metadata(&full).is_ok_and(|m| m.len() <= highlight::MAX_BYTES as u64);
                            small.then(|| std::fs::read(&full).ok()).flatten().map(|b| String::from_utf8_lossy(&b).into_owned())
                        }
                    })
                });
                Msg::BlameSyntax(p, generation, syntax)
            });
        }
        self.spawn_stream(move |send| {
            let ignore = git::ignore_revs(&p);
            let text = git::blame_text(&p, rev.as_deref(), &file);
            let ok = text.is_ok();
            send(Msg::BlameText(p.clone(), generation, text, ignore.clone()));
            if !ok {
                return Msg::BlameDone(p, generation, Ok(()));
            }
            // Results go to the screen in batches, a few times a second.
            let mut sent = 0;
            let mut batch = Vec::new();
            let mut last = std::time::Instant::now();
            let res = git::blame(&p, rev.as_deref(), &file, &opts, ignore.as_ref(), |parser, r| {
                batch.push(r);
                if last.elapsed() >= std::time::Duration::from_millis(50) {
                    send(Msg::BlameChunk(p.clone(), generation, parser.commits[sent..].to_vec(), std::mem::take(&mut batch)));
                    sent = parser.commits.len();
                    last = std::time::Instant::now();
                }
            });
            match res {
                Ok(commits) => {
                    if !batch.is_empty() {
                        send(Msg::BlameChunk(p.clone(), generation, commits[sent..].to_vec(), batch));
                    }
                    Msg::BlameDone(p, generation, Ok(()))
                }
                Err(e) => Msg::BlameDone(p, generation, Err(e)),
            }
        });
    }

    /// Read the commits for the file view's history.
    fn load_history(&mut self, path: &Path) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        rv.file_gen += 1;
        let generation = rv.file_gen;
        let Some(h) = rv.file_view.as_mut().and_then(|f| f.history.as_mut()) else { return };
        h.generation = generation;
        let (p, kind, file) = (path.to_path_buf(), h.kind.clone(), h.path.clone());
        self.spawn(move || {
            let without_diffs = |l: Vec<git::FileCommit>| l.into_iter().map(|c| (c, None)).collect();
            let res = match kind {
                fileview::HistoryKind::File => git::file_log(&p, &file, false, fileview::HISTORY_LIMIT).map(without_diffs),
                fileview::HistoryKind::Folder => git::file_log(&p, &file, true, fileview::HISTORY_LIMIT).map(without_diffs),
                fileview::HistoryKind::Lines { rev: Some(rev), start, end } => git::line_log(&p, &rev, &file, start, end, fileview::LINE_HISTORY_LIMIT),
                fileview::HistoryKind::Lines { rev: None, start, end } => git::working_line_log(&p, &file, start, end, fileview::LINE_HISTORY_LIMIT),
            };
            Msg::FileLog(p, generation, res)
        });
    }

    /// Pick commit `i` in the file view's history and show what it changed there.
    fn select_history(&mut self, path: &Path, i: usize) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        rv.file_gen += 1;
        let generation = rv.file_gen;
        let Some(h) = rv.file_view.as_mut().and_then(|f| f.history.as_mut()) else { return };
        let Some((c, diff)) = h.commits().get(i).cloned() else { return };
        h.selected = Some(i);
        h.diff_generation = generation;
        h.diff_scroll = Some(0);
        if let fileview::HistoryKind::Lines { .. } = h.kind {
            // Line history came with its changes.
            let mut doc = DiffDoc::new(diff.into_iter().collect(), true);
            doc.commit = Some(c.hash.clone());
            doc.set_split(self.cfg.view.diff.split);
            h.diff = Some(Ok(doc));
            return;
        }
        h.diff = None;
        let paths = match h.kind {
            fileview::HistoryKind::Folder => vec![h.path.clone()],
            _ => std::iter::once(c.path.clone()).chain(c.old_path.clone()).collect(),
        };
        let p = path.to_path_buf();
        let (ws, split) = (self.cfg.view.diff.ignore_whitespace, self.cfg.view.diff.split);
        self.spawn_stream(move |send| {
            let doc = git::commit_files(&p, &c.hash, &paths, ws).map(|files| {
                let mut doc = DiffDoc::new(files, true);
                doc.whitespace_ignored = ws;
                doc.set_split(split);
                doc.load_previews(&p, Some(&c.hash));
                doc
            });
            let job = doc.as_ref().ok().map(|d| d.highlighter(&p));
            send(Msg::FileDiff(p, generation, doc));
            if let Some(job) = job {
                job.run(&|| send(Msg::Highlighted));
            }
            Msg::Highlighted
        });
    }

    /// Open, change or move around in the file view of the repository at `path`.
    fn file_action(&mut self, path: &Path, op: FileOpen) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        match op {
            FileOpen::Blame { rev, path: file, line, back } => {
                let fv = rv.file_view.get_or_insert_with(|| FileView::new(Mode::Blame));
                let top = fv.top_line();
                if back {
                    if let Some(b) = &fv.blame {
                        fv.back.push((b.rev.clone(), b.path.clone(), top));
                    }
                } else {
                    fv.back.clear();
                }
                if fv.history.as_ref().is_some_and(|h| h.path != file) {
                    fv.history = None;
                }
                fv.mode = Mode::Blame;
                fv.shown = true;
                fv.blame = Some(fileview::Blame::new(rev, file, 0, line));
                self.load_blame(path);
            }
            FileOpen::History { path: file, folder } => {
                let kind = if folder { fileview::HistoryKind::Folder } else { fileview::HistoryKind::File };
                self.open_history(path, kind, file);
            }
            FileOpen::Lines { rev, path: file, start, end } => self.open_history(path, fileview::HistoryKind::Lines { rev, start, end }, file),
            FileOpen::Mode(m) => {
                let Some(fv) = rv.file_view.as_mut() else { return };
                fv.mode = m;
                fv.shown = true;
                match m {
                    Mode::Blame if fv.blame.is_none() => {
                        let Some(h) = &fv.history else { return };
                        let (rev, line) = match &h.kind {
                            fileview::HistoryKind::Folder => return,
                            fileview::HistoryKind::Lines { rev, start, .. } => (rev.clone(), Some(*start)),
                            fileview::HistoryKind::File => (None, None),
                        };
                        fv.blame = Some(fileview::Blame::new(rev, h.path.clone(), 0, line));
                        self.load_blame(path);
                    }
                    Mode::History if fv.history.is_none() => {
                        let Some(b) = &fv.blame else { return };
                        fv.history = Some(fileview::History::new(fileview::HistoryKind::File, b.path.clone(), 0));
                        self.load_history(path);
                    }
                    _ => {}
                }
            }
            FileOpen::Reload => {
                let Some(fv) = rv.file_view.as_mut() else { return };
                let top = fv.top_line();
                let Some(b) = &fv.blame else { return };
                let mut fresh = fileview::Blame::new(b.rev.clone(), b.path.clone(), 0, None);
                fresh.sel = b.sel;
                fresh.scroll_to = Some(top);
                fv.blame = Some(fresh);
                self.load_blame(path);
            }
            FileOpen::Back => {
                let Some(fv) = rv.file_view.as_mut() else { return };
                let Some((rev, file, top)) = fv.back.pop() else { return };
                if fv.history.as_ref().is_some_and(|h| h.path != file) {
                    fv.history = None;
                }
                let mut b = fileview::Blame::new(rev, file, 0, None);
                b.scroll_to = Some(top);
                fv.blame = Some(b);
                fv.mode = Mode::Blame;
                self.load_blame(path);
            }
            FileOpen::SelectCommit(i) => self.select_history(path, i),
            FileOpen::Show => {
                if let Some(fv) = rv.file_view.as_mut() {
                    fv.shown = true;
                }
            }
        }
    }

    fn open_history(&mut self, path: &Path, kind: fileview::HistoryKind, file: String) {
        let Some(rv) = self.tabs.get_mut(path) else { return };
        let fv = rv.file_view.get_or_insert_with(|| FileView::new(Mode::History));
        // Blame stays only for the same file (switching back to it keeps your place).
        let same = fv.blame.as_ref().is_some_and(|b| b.path == file) && kind != fileview::HistoryKind::Folder;
        if !same {
            fv.blame = None;
            fv.back.clear();
        }
        fv.mode = Mode::History;
        fv.shown = true;
        fv.history = Some(fileview::History::new(kind, file, 0));
        self.load_history(path);
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let now = self.ctx.input(|i| i.time);
        self.toasts.push(Toast { text: text.into(), error, until: now + if error { 8.0 } else { 3.5 }, undo: None });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// A notification with an Undo button (shown a little longer, to give time to click it).
    fn toast_undo(&mut self, text: impl Into<String>, repo: PathBuf, id: u64) {
        self.toast(text, false);
        if let Some(t) = self.toasts.last_mut() {
            t.until += 3.0;
            t.undo = Some((repo, id));
        }
    }

    fn run_git(&mut self, repo: PathBuf, label: String, op: GitFn, kind: OpKind, record: Option<Record>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.busy.push((id, repo.clone(), label.clone()));
        self.spawn(move || {
            // An undoable action saves what it changes first, on this same worker.
            let (result, undo) = match record {
                None => (op(&repo), None),
                Some(r) => match crate::oplog::run_recorded(&repo, &r.undo, &r.what, op) {
                    Ok((out, entry)) => (Ok(out), entry.map(|e| e.id)),
                    Err(e) => (Err(e), None),
                },
            };
            Msg::OpDone { id, repo, label, result, kind, undo }
        });
        id
    }

    /// Drop a folder from every workspace and close its tabs (it was deleted).
    fn forget_repo(&mut self, path: &Path) {
        for w in &mut self.cfg.workspaces {
            w.repos.retain(|p| p != path);
            if let Some(i) = w.open_tabs.iter().position(|p| p == path) {
                w.open_tabs.remove(i);
                w.active_tab = match w.active_tab {
                    Some(a) if a == i => None,
                    Some(a) if a > i => Some(a - 1),
                    other => other,
                };
            }
        }
        self.tabs.remove(path);
        self.summaries.remove(path);
        self.save();
    }

    fn save(&self) {
        self.cfg.save();
    }

    // -----------------------------------------------------------------------
    // Messages

    /// Nothing is running in the background (tests wait for this).
    #[cfg(test)]
    pub fn idle(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0 && self.busy.is_empty()
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            match msg {
                Msg::Highlighted => {}
                Msg::Repo(path, res) => self.on_repo_loaded(path, res),
                Msg::Summary(path, res) => {
                    self.summaries.insert(path, res);
                }
                Msg::Detail(path, hash, res) => {
                    if let Some(rv) = self.tabs.get_mut(&path) {
                        if rv.selection == Some(Selection::Commit(hash)) {
                            rv.detail_loading = false;
                            match res {
                                Ok(d) => {
                                    let other_commit = rv.detail.as_ref().map(|old| &old.info.hash) != Some(&d.info.hash);
                                    rv.detail = Some(d);
                                    if other_commit {
                                        rv.reset_detail_scroll = true;
                                        rv.detail_sel = LineSel::default();
                                    }
                                }
                                Err(e) => rv.error = Some(e),
                            }
                        }
                    }
                }
                Msg::WorkDiff(path, generation, doc) => {
                    if let Some(rv) = self.tabs.get_mut(&path) {
                        if rv.wd_gen == generation {
                            match doc {
                                Ok(mut doc) => {
                                    // Keep files folded/unfolded the way they were left.
                                    for fi in 0..doc.files.len() {
                                        if let Some(&folded) = rv.wd_folds.get(&doc.fold_key(fi)) {
                                            doc.set_collapsed(fi, folded);
                                        }
                                    }
                                    rv.wd_doc = Some(doc);
                                }
                                Err(e) => self.toasts.push(Toast { text: format!("Couldn't read the changes\n{e}"), error: true, until: self.ctx.input(|i| i.time) + 8.0, undo: None }),
                            }
                            rv.line_sel = LineSel::default();
                        }
                    }
                }
                Msg::Conflicts(path, generation, files) => {
                    if let Some(rv) = self.tabs.get_mut(&path)
                        && let Some(res) = rv.resolver.as_mut().filter(|_| rv.conflict_gen == generation)
                    {
                        res.loaded(files);
                    }
                }
                Msg::Reflog(path, generation, res) => {
                    if let Some(rv) = self.tabs.get_mut(&path).filter(|rv| rv.reflog_gen == generation) {
                        rv.reflog = Some(res);
                    }
                }
                Msg::BlameText(path, generation, text, ignore) => {
                    if let Some(b) = self.blame_mut(&path, generation) {
                        b.text_loaded(text, ignore);
                    }
                }
                Msg::BlameSyntax(path, generation, syntax) => {
                    if let Some(b) = self.blame_mut(&path, generation) {
                        b.syntax = syntax;
                    }
                }
                Msg::BlameChunk(path, generation, commits, ranges) => {
                    if let Some(b) = self.blame_mut(&path, generation) {
                        b.add(commits, ranges);
                    }
                }
                Msg::BlameDone(path, generation, res) => {
                    if let Some(b) = self.blame_mut(&path, generation) {
                        b.finished(res);
                    }
                }
                Msg::FileLog(path, generation, res) => {
                    let h = self.tabs.get_mut(&path).and_then(|rv| rv.file_view.as_mut()).and_then(|f| f.history.as_mut()).filter(|h| h.generation == generation);
                    if let Some(h) = h {
                        let any = matches!(&res, Ok(l) if !l.is_empty());
                        h.list = Some(res);
                        if any {
                            self.select_history(&path, 0);
                        }
                    }
                }
                Msg::FileDiff(path, generation, res) => {
                    let h = self.tabs.get_mut(&path).and_then(|rv| rv.file_view.as_mut()).and_then(|f| f.history.as_mut()).filter(|h| h.diff_generation == generation);
                    if let Some(h) = h {
                        h.diff = Some(res);
                    }
                }
                Msg::TrackedFiles(repo, res) => {
                    if let Some(Dialog::FindFile { repo: r, files, hits_for, .. }) = &mut self.dialog
                        && *r == repo
                    {
                        *files = Some(res);
                        *hits_for = None;
                    }
                }
                Msg::SamplesReady => {
                    self.samples_pending = false;
                    self.start();
                }
                Msg::Picked(dir, found) => {
                    self.picking = false;
                    self.add_picked(dir, found);
                }
                Msg::PickClosed => self.picking = false,
                Msg::PickedDest(d) => {
                    self.picking = false;
                    match &mut self.dialog {
                        Some(Dialog::Clone { dest, .. }) => *dest = d.display().to_string(),
                        Some(Dialog::CreateWorktree { repo, path, path_edited, new_branch, name, existing, .. }) => {
                            let branch = if *new_branch { name.as_str() } else { existing.as_str() };
                            let leaf = git::default_worktree_path(repo, branch);
                            *path = d.join(leaf.file_name().unwrap_or_default()).display().to_string();
                            *path_edited = true;
                        }
                        _ => {}
                    }
                }
                Msg::EditMessage { repo, hash, message } => match message {
                    Ok(message) => {
                        let data = self.tabs.get(&repo).and_then(|rv| rv.data.as_ref());
                        let pushed = data.is_some_and(|d| d.pushed.contains(&hash));
                        let is_head = data.is_some_and(|d| d.status.head.oid.as_deref() == Some(hash.as_str()));
                        self.dialog = Some(Dialog::EditMessage { repo, original: message.clone(), message, hash, pushed, is_head });
                    }
                    Err(e) => self.toast(format!("Couldn't read that commit's message\n{e}"), true),
                },
                Msg::LastMessage(path, msg) => {
                    if let Some(rv) = self.tabs.get_mut(&path) {
                        if rv.amend && rv.commit_msg.trim().is_empty() {
                            rv.commit_msg = msg;
                        }
                    }
                }
                Msg::OpDone { id, repo, label, result, kind, undo } => {
                    self.busy.retain(|b| b.0 != id);
                    let follow = self.followups.remove(&id);
                    match result {
                        Ok(out) => {
                            match follow {
                                Some(FollowUp::Open(p)) => self.add_repo(p, true),
                                Some(FollowUp::Forget(p)) => self.forget_repo(&p),
                                None => {}
                            }
                            if kind == OpKind::Commit {
                                if let Some(rv) = self.tabs.get_mut(&repo) {
                                    rv.commit_msg.clear();
                                    rv.amend = false;
                                }
                            }
                            if kind == OpKind::Reword {
                                // Keep the edited commit selected under its new id.
                                if let Some(rv) = self.tabs.get_mut(&repo) {
                                    rv.selection = Some(Selection::Commit(out.trim().to_string()));
                                    rv.detail = None;
                                }
                            }
                            if kind != OpKind::Quiet || undo.is_some() {
                                let extra = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
                                let msg = if kind == OpKind::Reword {
                                    "Commit message updated".to_string()
                                } else if kind == OpKind::Undo {
                                    out.trim().to_string()
                                } else if extra.len() < 90 && !extra.is_empty() && kind != OpKind::Commit && kind != OpKind::Quiet {
                                    format!("{label}: done — {}", extra.trim())
                                } else {
                                    format!("{label}: done")
                                };
                                match undo {
                                    Some(entry) => self.toast_undo(msg, repo.clone(), entry),
                                    None => self.toast(msg, false),
                                }
                            }
                        }
                        // Undo explains itself; git's hints would only confuse.
                        Err(e) if kind == OpKind::Undo => self.toast(format!("Couldn't undo\n{e}"), true),
                        Err(e) => self.toast(format!("{label} failed\n{}", friendly_error(&e)), true),
                    }
                    if self.tabs.contains_key(&repo) {
                        self.reload_repo(&repo);
                    }
                    self.reload_summary(&repo);
                }
                Msg::Cloned(id, res) => {
                    self.busy.retain(|b| b.0 != id);
                    match res {
                        Ok(path) => {
                            self.toast(format!("Cloned {}", git::repo_name(&path)), false);
                            self.add_repo(path, true);
                        }
                        Err(e) => self.toast(format!("Clone failed\n{}", friendly_error(&e)), true),
                    }
                }
            }
        }
    }

    /// The blame the file view of `path` shows, if it's still the one asked for.
    fn blame_mut(&mut self, path: &Path, generation: u64) -> Option<&mut fileview::Blame> {
        self.tabs.get_mut(path)?.file_view.as_mut()?.blame.as_mut().filter(|b| b.generation == generation)
    }

    fn on_repo_loaded(&mut self, path: PathBuf, res: Result<RepoData, String>) {
        let mut want_detail = None;
        let mut want_wd = false;
        let mut want_reflog = false;
        let mut want_conflicts = Vec::new();
        {
            let Some(rv) = self.tabs.get_mut(&path) else { return };
            rv.loading = false;
            match res {
                Err(e) => {
                    rv.error = Some(e);
                    rv.data = None;
                }
                Ok(data) => {
                    rv.error = None;
                    let changes = data.status.change_count();
                    // Pick a sensible selection on first load, and keep it valid later.
                    match &rv.selection {
                        None => {
                            rv.selection = if changes > 0 || data.commits.is_empty() {
                                Some(Selection::WorkingDir)
                            } else {
                                data.commits.first().map(|c| Selection::Commit(c.hash.clone()))
                            };
                        }
                        Some(Selection::Commit(h)) if !data.commits.iter().any(|c| &c.hash == h) => {
                            rv.selection = Some(Selection::WorkingDir);
                        }
                        _ => {}
                    }
                    match &rv.selection {
                        Some(Selection::Commit(h)) => {
                            if rv.detail.as_ref().map(|d| &d.info.hash) != Some(h) {
                                want_detail = Some(h.clone());
                            }
                        }
                        Some(Selection::WorkingDir) => {
                            let st = &data.status;
                            let in_list = |s: &FileSel| {
                                let list = if s.staged { &st.staged } else { &st.unstaged };
                                list.iter().any(|f| f.path == s.path)
                            };
                            let mut sel = rv.file_sel.clone();
                            if let Some(s) = &mut sel {
                                if !in_list(s) {
                                    s.staged = !s.staged;
                                    s.untracked = false;
                                    if !in_list(s) {
                                        sel = None;
                                    }
                                }
                            }
                            if sel.is_none() {
                                sel = first_file(st);
                            }
                            rv.file_sel = sel;
                            want_wd = true;
                        }
                        Some(Selection::Reflog) => {
                            // A deleted branch has no reflog left: go back to HEAD's.
                            let gone = rv.reflog_ref != "HEAD" && !data.branches.iter().any(|b| !b.remote && format!("refs/heads/{}", b.name) == rv.reflog_ref);
                            if gone {
                                rv.reflog_ref = "HEAD".into();
                            }
                            want_reflog = true;
                        }
                        None => {}
                    }
                    // Resolved files leave the resolver (keeping unsaved choices in the
                    // rest); it closes once nothing is conflicted.
                    if let Some(res) = &mut rv.resolver {
                        if data.status.conflicts.is_empty() {
                            rv.resolver = None;
                        } else {
                            want_conflicts = res.sync(&data.status.conflicts);
                        }
                    }
                    rv.data = Some(data);
                    rv.filter_key = None;
                }
            }
        }
        if let Some(h) = want_detail {
            self.load_detail(&path, h);
        }
        if want_wd {
            self.load_work_diff(&path);
        }
        if want_reflog {
            self.load_reflog(&path);
        }
        if !want_conflicts.is_empty() {
            self.load_conflicts(&path, want_conflicts);
        }
    }

    // -----------------------------------------------------------------------
    // Actions

    fn do_action(&mut self, action: Action) {
        let active = self.active_path();
        match action {
            Action::SelectTab(i) => {
                self.cfg.ws_mut().active_tab = i;
                self.save();
                match self.active_path() {
                    Some(p) => {
                        if self.tabs.contains_key(&p) {
                            self.reload_repo(&p);
                        } else {
                            self.ensure_tab(&p);
                        }
                    }
                    None => self.refresh_overview(),
                }
            }
            Action::OpenRepo(path) => {
                let ws = self.cfg.ws_mut();
                let idx = match ws.open_tabs.iter().position(|p| *p == path) {
                    Some(i) => i,
                    None => {
                        ws.open_tabs.push(path.clone());
                        ws.open_tabs.len() - 1
                    }
                };
                self.do_action(Action::SelectTab(Some(idx)));
            }
            Action::CloseTab(i) => {
                let ws = self.cfg.ws_mut();
                if i < ws.open_tabs.len() {
                    let p = ws.open_tabs.remove(i);
                    ws.active_tab = match ws.active_tab {
                        Some(a) if a == i => {
                            if ws.open_tabs.is_empty() { None } else { Some(i.min(ws.open_tabs.len() - 1)) }
                        }
                        Some(a) if a > i => Some(a - 1),
                        other => other,
                    };
                    if !self.cfg.workspaces.iter().any(|w| w.open_tabs.contains(&p)) {
                        self.tabs.remove(&p);
                    }
                    self.save();
                    if let Some(p) = self.active_path() {
                        self.ensure_tab(&p);
                    }
                }
            }
            Action::SwitchWorkspace(i) => {
                if i < self.cfg.workspaces.len() {
                    self.cfg.current = i;
                    self.save();
                    self.refresh_overview();
                    if let Some(p) = self.active_path() {
                        self.ensure_tab(&p);
                    }
                }
            }
            Action::RemoveRepo(path) => {
                let ws = self.cfg.ws_mut();
                ws.repos.retain(|p| *p != path);
                if let Some(i) = ws.open_tabs.iter().position(|p| *p == path) {
                    self.do_action(Action::CloseTab(i));
                }
                self.save();
                self.toast(format!("Removed {} from this workspace (files are untouched)", git::repo_name(&path)), false);
            }
            Action::Dialog(d) => self.dialog = Some(d),
            Action::Refresh => match active {
                Some(p) => {
                    self.reload_repo(&p);
                    self.reload_summary(&p);
                }
                None => self.refresh_overview(),
            },
            Action::RefreshOverview => self.refresh_overview(),
            Action::DiffPrefsChanged { reload } => {
                self.save();
                let split = self.cfg.view.diff.split;
                for rv in self.tabs.values_mut() {
                    let history = rv.file_view.as_mut().and_then(|f| f.history.as_mut()).and_then(|h| h.diff.as_mut()).and_then(|d| d.as_mut().ok());
                    for doc in [rv.detail.as_mut().map(|d| &mut d.doc), rv.wd_doc.as_mut(), history].into_iter().flatten() {
                        doc.set_split(split);
                    }
                }
                if !reload {
                    return;
                }
                let Some(p) = active else { return };
                let Some(rv) = self.tabs.get(&p) else { return };
                let commit = match &rv.selection {
                    Some(Selection::Commit(h)) => Some(h.clone()),
                    _ => None,
                };
                let wd = rv.wd_doc.is_some() || rv.selection == Some(Selection::WorkingDir);
                let history = rv.file_view.as_ref().and_then(|f| f.history.as_ref()).and_then(|h| h.selected);
                if let Some(h) = commit {
                    self.load_detail(&p, h);
                }
                if wd {
                    self.load_work_diff(&p);
                }
                if let Some(i) = history {
                    self.select_history(&p, i);
                }
            }
            Action::SelectCommit(hash) => {
                let Some(p) = active else { return };
                if let Some(rv) = self.tabs.get_mut(&p) {
                    if rv.selection.as_ref() == Some(&Selection::Commit(hash.clone())) {
                        return;
                    }
                    rv.selection = Some(Selection::Commit(hash.clone()));
                    rv.detail = None;
                }
                self.load_detail(&p, hash);
            }
            Action::SelectRef(name) => {
                let Some(p) = active.clone() else { return };
                let found = self.tabs.get(&p).and_then(|rv| {
                    let data = rv.data.as_ref()?;
                    data.commits.iter().position(|c| c.refs.iter().any(|r| r.name == name)).map(|i| (i, data.commits[i].hash.clone()))
                });
                match found {
                    Some((i, hash)) => {
                        if let Some(rv) = self.tabs.get_mut(&p) {
                            rv.search.clear();
                            rv.list_scroll_to = Some(i);
                        }
                        self.do_action(Action::SelectCommit(hash));
                    }
                    None => {
                        let hidden = self.tabs.get(&p).and_then(|rv| rv.data.as_ref()).is_some_and(|d| {
                            d.hidden.iter().any(|h| h == &format!("refs/heads/{name}") || h == &format!("refs/remotes/{name}"))
                        });
                        let msg = if hidden {
                            format!("{name} is hidden. Click its eye icon to show it in the history.")
                        } else {
                            format!("{name} is not in the loaded history")
                        };
                        self.toast(msg, false)
                    }
                }
            }
            Action::SelectWorkingDir => {
                let Some(p) = active else { return };
                if let Some(rv) = self.tabs.get_mut(&p) {
                    rv.selection = Some(Selection::WorkingDir);
                    if let Some(d) = &rv.data {
                        if rv.file_sel.is_none() {
                            rv.file_sel = first_file(&d.status);
                        }
                    }
                }
                self.load_work_diff(&p);
            }
            Action::ShowReflog(name) => {
                let Some(p) = active else { return };
                let Some(rv) = self.tabs.get_mut(&p) else { return };
                rv.selection = Some(Selection::Reflog);
                if let Some(n) = name {
                    if n != rv.reflog_ref {
                        rv.reflog_ref = n;
                        rv.reflog = None;
                    }
                }
                self.load_reflog(&p);
            }
            Action::File(op) => {
                if let Some(p) = active {
                    self.file_action(&p, op);
                }
            }
            Action::CloseFile => {
                if let Some(rv) = active.and_then(|p| self.tabs.get_mut(&p)) {
                    rv.file_view = None;
                }
            }
            Action::ShowCommit(hash) => {
                let Some(p) = active else { return };
                let Some(rv) = self.tabs.get_mut(&p) else { return };
                if let Some(fv) = rv.file_view.as_mut() {
                    fv.shown = false;
                }
                let found = rv.data.as_ref().and_then(|d| d.commits.iter().position(|c| c.hash == hash));
                match found {
                    Some(i) => {
                        rv.search.clear();
                        rv.list_scroll_to = Some(i);
                    }
                    None => self.toast("That commit isn't in the loaded history (its branch may be hidden). Showing its details.", false),
                }
                self.do_action(Action::SelectCommit(hash));
            }
            Action::ShowWorkingDir => {
                if let Some(fv) = active.and_then(|p| self.tabs.get_mut(&p)).and_then(|rv| rv.file_view.as_mut()) {
                    fv.shown = false;
                }
                self.do_action(Action::SelectWorkingDir);
            }
            Action::FindFile => {
                let Some(p) = active else { return };
                self.dialog = Some(Dialog::FindFile { repo: p.clone(), query: String::new(), files: None, hits: Vec::new(), hits_for: None, picked: 0 });
                self.spawn(move || Msg::TrackedFiles(p.clone(), git::tracked_files(&p).map(|f| fileview::with_folders(&f))));
            }
            Action::SelectFile(sel) => {
                // Highlight it in the list, unfold it and scroll the stacked view to it.
                let Some(p) = active else { return };
                if let Some(rv) = self.tabs.get_mut(&p) {
                    if let Some(doc) = rv.wd_doc.as_mut() {
                        let want = if sel.untracked {
                            views::DiffMode::Untracked
                        } else if sel.staged {
                            views::DiffMode::Staged
                        } else {
                            views::DiffMode::Unstaged
                        };
                        if let Some(fi) = (0..doc.files.len()).find(|&i| doc.files[i].path == sel.path && doc.modes[i] == want) {
                            if doc.is_collapsed(fi) {
                                doc.set_collapsed(fi, false);
                                rv.wd_folds.insert(doc.fold_key(fi), false);
                                rv.line_sel = LineSel::default();
                            }
                            rv.diff_scroll_to = doc.file_rows.get(fi).map(|r| r.saturating_sub(1));
                        }
                    }
                    rv.file_sel = Some(sel);
                }
            }
            Action::OpenConflict(file) => {
                let Some(p) = active else { return };
                let Some(rv) = self.tabs.get_mut(&p) else { return };
                let Some(conflicts) = rv.data.as_ref().map(|d| d.status.conflicts.clone()) else { return };
                // Coming back keeps the choices made earlier.
                if let Some(res) = &mut rv.resolver {
                    res.shown = true;
                    if let Some(i) = res.files.iter().position(|f| f.path == file) {
                        res.select(i);
                    }
                    return;
                }
                let mut res = Resolver::new(&conflicts, &file);
                let load = res.sync(&conflicts);
                rv.resolver = Some(res);
                self.load_conflicts(&p, load);
            }
            Action::CloseResolver => {
                let Some(p) = active else { return };
                if let Some(res) = self.tabs.get_mut(&p).and_then(|rv| rv.resolver.as_mut()) {
                    res.shown = false;
                }
            }
            Action::Git { repo, label, op, kind, record } => {
                self.run_git(repo, label, op, kind, record);
            }
            Action::GitThen { repo, label, op, then } => {
                let id = self.run_git(repo, label, op, OpKind::Normal, None);
                self.followups.insert(id, then);
            }
            Action::Undo { repo, id } => {
                // One at a time: a second press would race the first.
                if !self.busy.iter().any(|b| b.1 == repo && b.2 == "Undo") {
                    self.run_git(repo, "Undo".into(), Box::new(move |r| crate::oplog::undo(r, id)), OpKind::Undo, None);
                }
            }
            Action::OpenWorktree(path) => self.add_repo(path, true),
            Action::ToggleHidden(full) => {
                let Some(p) = active else { return };
                let list = self.cfg.hidden_refs.entry(p.clone()).or_default();
                match list.iter().position(|h| *h == full) {
                    Some(i) => {
                        list.remove(i);
                    }
                    None => list.push(full),
                }
                if list.is_empty() {
                    self.cfg.hidden_refs.remove(&p);
                }
                self.save();
                self.reload_repo(&p);
            }
            Action::EditMessage(hash) => {
                let Some(p) = active else { return };
                self.spawn(move || {
                    let message = git::run(&p, &["log", "-1", "--format=%B", &hash]).map(|m| m.trim_end().to_string());
                    Msg::EditMessage { repo: p, hash, message }
                });
            }
            Action::ShowAllBranches => {
                let Some(p) = active else { return };
                self.cfg.hidden_refs.remove(&p);
                self.save();
                self.reload_repo(&p);
            }
            Action::LoadMore => {
                let Some(p) = active else { return };
                if let Some(rv) = self.tabs.get_mut(&p) {
                    rv.limit += PAGE * 3;
                }
                self.reload_repo(&p);
            }
            Action::PickRepo => self.pick_folder("Choose a repository folder", |dir| {
                let found = git::scan_for_repos(&dir);
                Msg::Picked(dir, found)
            }),
            Action::PickFolder => self.pick_folder("Choose a folder that contains repositories", |dir| {
                let found = git::scan_for_repos(&dir);
                Msg::Picked(dir, found)
            }),
            Action::Copy(text) => {
                self.ctx.copy_text(text);
                self.toast("Copied to clipboard", false);
            }
            Action::ToggleAmend => {
                let Some(p) = active else { return };
                if let Some(rv) = self.tabs.get_mut(&p) {
                    rv.amend = !rv.amend;
                    if rv.amend && rv.commit_msg.trim().is_empty() {
                        self.spawn(move || {
                            let m = git::last_message(&p);
                            Msg::LastMessage(p, m)
                        });
                    }
                }
            }
            Action::FetchAll => {
                for r in self.cfg.ws().repos.clone() {
                    let a = Action::args(&r, format!("Fetch {}", git::repo_name(&r)), &["fetch", "--all", "--prune"]);
                    if let Action::Git { repo, label, op, .. } = a {
                        let has_remote = matches!(self.summaries.get(&repo), Some(Ok(s)) if s.has_remote);
                        if has_remote {
                            self.run_git(repo, label, op, OpKind::Quiet, None);
                        }
                    }
                }
                self.toast("Fetching every repository in this workspace…", false);
            }
            Action::PullAll => {
                let mut skipped = 0;
                for r in self.cfg.ws().repos.clone() {
                    let ok = matches!(self.summaries.get(&r), Some(Ok(s)) if s.has_remote && s.status.head.upstream.is_some());
                    if ok {
                        let a = Action::args(&r, format!("Pull {}", git::repo_name(&r)), &["pull", "--ff-only"]);
                        self.do_action(a.undoable(git::Undoable::Tree { save_work: false }, "pull"));
                    } else {
                        skipped += 1;
                    }
                }
                if skipped > 0 {
                    self.toast(format!("{skipped} repositories skipped (no remote branch to pull from)"), false);
                }
            }
        }
    }

    fn add_picked(&mut self, dir: PathBuf, found: Vec<PathBuf>) {
        if found.is_empty() {
            self.dialog = Some(Dialog::Confirm {
                title: "No repository here".into(),
                body: format!("{} is not a git repository and has none inside it.\n\nCreate a new, empty repository in this folder?", dir.display()),
                button: "Create repository".into(),
                danger: false,
                then: Some(Box::new(Action::Git {
                    repo: dir.clone(),
                    label: format!("Create repository {}", git::repo_name(&dir)),
                    op: Box::new(|p| git::run(p, &["init"])),
                    kind: OpKind::Normal,
                    record: None,
                })),
            });
            // Added after init so it shows up straight away.
            if !self.cfg.ws().repos.contains(&dir) {
                self.cfg.ws_mut().repos.push(dir);
                self.save();
            }
            return;
        }
        let n = found.len();
        let single = n == 1;
        for p in found {
            self.add_repo(p, single);
        }
        if !single {
            self.toast(format!("Added {n} repositories to this workspace"), false);
        }
    }

    fn add_repo(&mut self, path: PathBuf, open: bool) {
        if !self.cfg.ws().repos.contains(&path) {
            self.cfg.ws_mut().repos.push(path.clone());
            self.save();
        }
        self.reload_summary(&path);
        if open {
            self.do_action(Action::OpenRepo(path));
        }
    }

    // -----------------------------------------------------------------------
    // UI pieces

    fn workspace_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let ws = self.cfg.ws().clone();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Workspace switcher
            let label = RichText::new(format!("▦  {}  ⏷", ws.name)).strong().color(TEXT);
            ui.menu_button(label, |ui| {
                ui.set_min_width(220.0);
                ui.label(RichText::new("WORKSPACES").small().color(DIM));
                for (i, w) in self.cfg.workspaces.iter().enumerate() {
                    let text = format!("{}   ({} repos)", w.name, w.repos.len());
                    if ui.selectable_label(i == self.cfg.current, text).clicked() {
                        actions.push(Action::SwitchWorkspace(i));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("New workspace…").clicked() {
                    actions.push(Action::Dialog(Dialog::NewWorkspace { name: String::new() }));
                    ui.close();
                }
                if ui.button("Rename this workspace…").clicked() {
                    actions.push(Action::Dialog(Dialog::RenameWorkspace { name: ws.name.clone() }));
                    ui.close();
                }
                if ui.add_enabled(self.cfg.workspaces.len() > 1, egui::Button::new("Delete this workspace…")).clicked() {
                    actions.push(Action::Dialog(Dialog::DeleteWorkspace));
                    ui.close();
                }
            });
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 20.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, BORDER);
            ui.add_space(6.0);

            // Tabs
            if tab_button(ui, "⌂  Overview", ws.active_tab.is_none(), None).0.clicked() {
                actions.push(Action::SelectTab(None));
            }
            for (i, p) in ws.open_tabs.iter().enumerate() {
                let dirty = match self.summaries.get(p) {
                    Some(Ok(s)) => s.status.change_count() > 0,
                    _ => false,
                };
                let (resp, close) = tab_button(ui, &git::repo_name(p), ws.active_tab == Some(i), Some(dirty));
                if close {
                    actions.push(Action::CloseTab(i));
                } else if resp.clicked() {
                    actions.push(Action::SelectTab(Some(i)));
                } else if resp.middle_clicked() {
                    actions.push(Action::CloseTab(i));
                }
                resp.on_hover_text(p.display().to_string());
            }
            add_menu(ui, "+", actions);
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            if let Some((_, p, label)) = self.busy.last() {
                ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                let more = if self.busy.len() > 1 { format!("  (+{} more)", self.busy.len() - 1) } else { String::new() };
                ui.label(RichText::new(format!("{label} — {}{more}", git::repo_name(p))).small().color(DIM));
            } else {
                ui.label(RichText::new("● Ready").small().color(FAINT));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ws = self.cfg.ws();
                ui.label(RichText::new(format!("{} · {} repositories", ws.name, ws.repos.len())).small().color(FAINT));
                if let Some(p) = self.active_path() {
                    if let Some(b) = self.tabs.get(&p).and_then(|rv| rv.head_branch()) {
                        ui.label(RichText::new(format!("⎇ {b}")).small().color(DIM));
                    }
                    ui.label(RichText::new(p.display().to_string()).small().color(FAINT));
                }
            });
        });
    }

    fn toasts_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        egui::Area::new(Id::unique("toasts"))
            // Bottom-left: the commit message and Commit button live at the bottom right,
            // and a notification must never sit on top of them.
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(16.0, -34.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let mut dismiss = None;
                for (i, t) in self.toasts.iter().enumerate() {
                    let edge = if t.error { DANGER } else { ACCENT };
                    let r = egui::Frame::new()
                        .fill(RAISED)
                        .stroke(Stroke::new(1.0, edge))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            let icon = if t.error { "⚠" } else { "✔" };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(icon).color(edge));
                                ui.label(RichText::new(&t.text).color(TEXT));
                                if let Some((repo, id)) = &t.undo {
                                    if ui.button("Undo").clicked() {
                                        actions.push(Action::Undo { repo: repo.clone(), id: Some(*id) });
                                        dismiss = Some(i);
                                    }
                                }
                            });
                        });
                    // Click to dismiss (not when there's a button: the click would go here instead).
                    if t.undo.is_none() && r.response.interact(Sense::click()).clicked() {
                        dismiss = Some(i);
                    }
                    ui.add_space(6.0);
                }
                if let Some(i) = dismiss {
                    self.toasts.remove(i);
                }
            });
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let modal = egui::Modal::new(Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(420.0);
            match &mut dialog {
                Dialog::NewWorkspace { name } => {
                    if name_dialog(ui, "New workspace", name, enter, &mut keep) {
                        self.pending_name = Some(name.trim().to_string());
                    }
                }
                Dialog::RenameWorkspace { name } => {
                    if name_dialog(ui, "Rename workspace", name, enter, &mut keep) {
                        self.pending_name = Some(name.trim().to_string());
                    }
                }
                Dialog::DeleteWorkspace => {
                    ui.heading("Delete workspace?");
                    ui.label(format!(
                        "“{}” will be removed from Headway. Your repositories and their files stay exactly where they are.",
                        self.cfg.ws().name
                    ));
                    ui.add_space(8.0);
                    let (ok, cancel) = dialog_buttons(ui, "Delete workspace", true, true);
                    if ok {
                        let i = self.cfg.current;
                        self.cfg.workspaces.remove(i);
                        self.cfg.current = 0;
                        self.save();
                        actions.push(Action::SwitchWorkspace(0));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::AddPath { path } => {
                    ui.heading("Add repository");
                    ui.label(RichText::new("Type or paste the folder of a repository, or a folder that contains several.").color(DIM));
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(path).hint_text("/home/me/projects/my-app").desired_width(f32::INFINITY))
                        .request_focus();
                    ui.add_space(8.0);
                    let valid = Path::new(path.trim()).is_dir();
                    let (ok, cancel) = dialog_buttons(ui, "Add", valid, false);
                    if ok || (enter && valid) {
                        self.pending_add = Some(PathBuf::from(path.trim()));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::Clone { url, dest } => {
                    ui.heading("Clone repository");
                    ui.add_space(4.0);
                    ui.label("Address of the repository");
                    ui.add(egui::TextEdit::singleline(url).hint_text("https://example.com/team/project.git").desired_width(f32::INFINITY));
                    ui.add_space(4.0);
                    ui.label("Save it inside this folder");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(dest).desired_width(300.0));
                        if ui.button("Browse…").clicked() {
                            self.pick_folder("Choose where to save the repository", Msg::PickedDest);
                        }
                    });
                    ui.add_space(8.0);
                    let valid = !url.trim().is_empty() && Path::new(dest.trim()).is_dir();
                    let (ok, cancel) = dialog_buttons(ui, "Clone", valid, false);
                    if ok {
                        let id = self.next_id;
                        self.next_id += 1;
                        let (u, d) = (url.trim().to_string(), PathBuf::from(dest.trim()));
                        self.busy.push((id, d.clone(), "Cloning".into()));
                        self.spawn(move || Msg::Cloned(id, git::clone(&u, &d)));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::CreateBranch { repo, at, label, name, checkout } => {
                    ui.heading("New branch");
                    ui.label(RichText::new(format!("Starting from {label}")).color(DIM));
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(name).hint_text("feature/my-change").desired_width(f32::INFINITY))
                        .request_focus();
                    ui.checkbox(checkout, "Switch to the new branch");
                    ui.add_space(8.0);
                    let valid = valid_ref_name(name);
                    let (ok, cancel) = dialog_buttons(ui, "Create branch", valid, false);
                    if ok || (enter && valid) {
                        let args: Vec<String> = if *checkout {
                            vec!["checkout".into(), "-b".into(), name.trim().into(), at.clone()]
                        } else {
                            vec!["branch".into(), name.trim().into(), at.clone()]
                        };
                        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        actions.push(Action::args(repo, format!("Create branch {}", name.trim()), &a));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::CreateTag { repo, at, label, name } => {
                    ui.heading("New tag");
                    ui.label(RichText::new(format!("On {label}")).color(DIM));
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(name).hint_text("v1.2.0").desired_width(f32::INFINITY)).request_focus();
                    ui.add_space(8.0);
                    let valid = valid_ref_name(name);
                    let (ok, cancel) = dialog_buttons(ui, "Create tag", valid, false);
                    if ok || (enter && valid) {
                        actions.push(Action::args(repo, format!("Create tag {}", name.trim()), &["tag", name.trim(), at]));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::CreateWorktree { repo, free_branches, new_branch, existing, name, start, start_label, path, path_edited } => {
                    ui.heading("New worktree");
                    ui.label(
                        RichText::new("A worktree is a second folder for the same repository, with its own branch checked out. Work on two branches side by side without stashing.")
                            .color(DIM),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.radio_value(new_branch, true, "New branch");
                        ui.add_enabled_ui(!free_branches.is_empty(), |ui| {
                            ui.radio_value(new_branch, false, "Existing branch");
                        });
                    });
                    if *new_branch {
                        ui.add(egui::TextEdit::singleline(name).hint_text("feature/my-change").desired_width(f32::INFINITY));
                        ui.label(RichText::new(format!("Starting from {start_label}")).small().color(DIM));
                    } else {
                        if existing.is_empty() {
                            if let Some(b) = free_branches.first() {
                                *existing = b.clone();
                            }
                        }
                        let picker = egui::ComboBox::from_id_salt("wt_branch").selected_text(existing.as_str()).width(300.0).show_ui(ui, |ui| {
                            for b in free_branches.iter() {
                                ui.selectable_value(existing, b.clone(), b);
                            }
                        });
                        let picked = format!("Branch for the worktree: {existing}");
                        picker.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &picked));
                    }
                    let branch = if *new_branch { name.trim().to_string() } else { existing.clone() };
                    if !*path_edited {
                        *path = if branch.is_empty() { String::new() } else { git::default_worktree_path(repo, &branch).display().to_string() };
                    }
                    ui.add_space(6.0);
                    ui.label("Folder for the worktree");
                    let mut browse = false;
                    ui.horizontal(|ui| {
                        if ui.add(egui::TextEdit::singleline(path).desired_width(300.0)).changed() {
                            *path_edited = true;
                        }
                        browse = ui.button("Browse…").clicked();
                    });
                    let target = PathBuf::from(path.trim());
                    let taken = target.exists() && std::fs::read_dir(&target).map(|mut d| d.next().is_some()).unwrap_or(true);
                    if taken {
                        ui.label(RichText::new("That folder already exists and isn't empty.").small().color(WARN));
                    }
                    ui.add_space(8.0);
                    let valid = valid_ref_name(&branch) && !path.trim().is_empty() && !taken;
                    let (ok, cancel) = dialog_buttons(ui, "Create worktree", valid, false);
                    if browse {
                        self.pick_folder("Choose where to put the worktree", Msg::PickedDest);
                    }
                    if ok && valid {
                        let from = new_branch.then(|| start.clone());
                        let b = branch.clone();
                        let t = target.clone();
                        actions.push(Action::GitThen {
                            repo: repo.clone(),
                            label: format!("Create worktree for {branch}"),
                            op: Box::new(move |r| git::add_worktree(r, &t, &b, from.as_deref())),
                            then: FollowUp::Open(target),
                        });
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::AddSubmodule { repo, url, path, path_edited } => {
                    ui.heading("Add submodule");
                    ui.label(
                        RichText::new("A submodule puts another repository inside this one, pinned to a specific commit. Good for shared libraries.")
                            .color(DIM),
                    );
                    ui.add_space(8.0);
                    ui.label("Address of the repository to include");
                    let u = ui.add(egui::TextEdit::singleline(url).hint_text("https://example.com/team/shared-lib.git").desired_width(f32::INFINITY));
                    if url.is_empty() {
                        u.request_focus();
                    }
                    if !*path_edited {
                        *path = git::submodule_folder(url);
                    }
                    ui.add_space(4.0);
                    ui.label("Folder inside this repository");
                    if ui.add(egui::TextEdit::singleline(path).hint_text("libs/shared-lib").desired_width(f32::INFINITY)).changed() {
                        *path_edited = true;
                    }
                    let target = repo.join(path.trim());
                    let taken = !path.trim().is_empty() && target.exists();
                    let outside = Path::new(path.trim()).is_absolute() || path.split(['/', '\\']).any(|c| c == "..");
                    if taken {
                        ui.label(RichText::new("Something already exists at that folder.").small().color(WARN));
                    } else if outside {
                        ui.label(RichText::new("The folder has to be inside this repository.").small().color(WARN));
                    }
                    ui.label(RichText::new("It's cloned now and staged, ready for you to commit.").small().color(DIM));
                    ui.add_space(8.0);
                    let valid = !url.trim().is_empty() && !path.trim().is_empty() && !taken && !outside;
                    let (ok, cancel) = dialog_buttons(ui, "Add submodule", valid, false);
                    if (ok || enter) && valid {
                        let (u, p) = (url.trim().to_string(), path.trim().to_string());
                        actions.push(Action::git(repo, format!("Add submodule {p}"), move |r| git::add_submodule(r, &u, &p)));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::EditMessage { repo, hash, message, original, pushed, is_head } => {
                    ui.set_width(520.0);
                    ui.heading("Edit commit message");
                    ui.label(RichText::new(format!("Commit {}", &hash[..8.min(hash.len())])).monospace().color(DIM));
                    ui.add_space(6.0);
                    let te = egui::TextEdit::multiline(message)
                        .hint_text("Summary on the first line\n\nMore detail below")
                        .desired_rows(7)
                        .desired_width(f32::INFINITY);
                    let r = ui.add(te);
                    if !r.has_focus() && message == original {
                        r.request_focus();
                    }
                    ui.add_space(4.0);
                    if !*is_head {
                        ui.label(RichText::new("The commits after this one are replayed on top of it, so they get new ids too. Their contents don't change.").small().color(DIM));
                    }
                    if *pushed {
                        ui.label(
                            RichText::new("⚠ This commit is already on a remote. After editing, you'll need to force-push, and anyone who pulled it will have to catch up.")
                                .small()
                                .color(WARN),
                        );
                    }
                    ui.add_space(8.0);
                    let changed = message.trim() != original.trim();
                    let valid = !message.trim().is_empty() && changed;
                    let save_keys = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                    let (ok, cancel) = dialog_buttons(ui, "Save message", valid, false);
                    if (ok || save_keys) && valid {
                        let (h, m) = (hash.clone(), message.trim_end().to_string());
                        actions.push(Action::Git {
                            repo: repo.clone(),
                            label: "Edit commit message".into(),
                            op: Box::new(move |r| git::reword(r, &h, &m)),
                            kind: OpKind::Reword,
                            record: Some(Record { undo: git::Undoable::Refs, what: format!("edit the message of {}", &hash[..hash.len().min(7)]) }),
                        });
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::AddRemote { repo, name, url, fetch, taken } => {
                    ui.heading("Add remote");
                    ui.label(RichText::new("A remote is another copy of this repository, usually on a server, that you fetch from and push to.").color(DIM));
                    ui.add_space(8.0);
                    ui.label("Name");
                    let name_box = ui.add(egui::TextEdit::singleline(name).hint_text(if taken.is_empty() { "origin" } else { "upstream" }).desired_width(f32::INFINITY));
                    if name.is_empty() && url.is_empty() {
                        name_box.request_focus();
                    }
                    ui.add_space(4.0);
                    ui.label("Address");
                    ui.add(egui::TextEdit::singleline(url).hint_text("https://example.com/team/project.git").desired_width(f32::INFINITY));
                    ui.checkbox(fetch, "Fetch its branches now");
                    let clash = taken.iter().any(|t| t == name.trim());
                    let bad_name = !name.trim().is_empty() && !git::valid_remote_name(name);
                    if clash {
                        ui.label(RichText::new("There's already a remote with that name.").small().color(WARN));
                    } else if bad_name {
                        ui.label(RichText::new("Use letters, numbers, dots, dashes or underscores.").small().color(WARN));
                    }
                    ui.add_space(8.0);
                    let valid = git::valid_remote_name(name) && !clash && !url.trim().is_empty() && !url.trim().contains(char::is_whitespace);
                    let (ok, cancel) = dialog_buttons(ui, "Add remote", valid, false);
                    if (ok || enter) && valid {
                        let (n, u, f) = (name.trim().to_string(), url.trim().to_string(), *fetch);
                        actions.push(Action::git(repo, format!("Add remote {n}"), move |r| git::add_remote(r, &n, &u, f)));
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
                Dialog::FindFile { query, files, hits, hits_for, picked, .. } => {
                    ui.set_width(560.0);
                    ui.heading("Find file");
                    ui.label(RichText::new("Blame a file, or see the history of a file or folder.").color(DIM));
                    ui.add_space(6.0);
                    // ↑/↓ move through the list below, so the box keeps them (egui would otherwise move focus away).
                    let keep_arrows = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
                    let edit = ui.add(egui::TextEdit::singleline(query).hint_text("Type part of a path").desired_width(f32::INFINITY).event_filter(keep_arrows));
                    edit.request_focus();
                    if edit.changed() {
                        *picked = 0;
                    }
                    ui.add_space(6.0);
                    let mut open = None;
                    match files {
                        None => {
                            ui.add(egui::Spinner::new().color(ACCENT));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(format!("Couldn't list the files\n{e}")).color(WARN));
                        }
                        Some(Ok(all)) => {
                            if hits_for.as_deref() != Some(query.as_str()) {
                                *hits = fileview::find_paths(all, query, 300);
                                *hits_for = Some(query.clone());
                            }
                            let (up, down, history_key) = ctx.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.modifiers.command));
                            if up {
                                *picked = picked.saturating_sub(1);
                            }
                            if down && *picked + 1 < hits.len() {
                                *picked += 1;
                            }
                            if hits.is_empty() {
                                ui.label(RichText::new("Nothing matches").color(FAINT));
                            }
                            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                                for (i, (path, folder)) in hits.iter().enumerate() {
                                    let label = if *folder { format!("{path}/") } else { path.clone() };
                                    let r = ui.add(egui::Button::selectable(*picked == i, RichText::new(label).monospace()));
                                    if (up || down) && *picked == i {
                                        r.scroll_to_me(None);
                                    }
                                    if r.clicked() {
                                        *picked = i;
                                    }
                                    if r.double_clicked() {
                                        open = Some((i, *folder));
                                    }
                                }
                            });
                            if enter && !hits.is_empty() {
                                // Enter blames a file; Ctrl/Cmd+Enter (or a folder) shows history.
                                open = Some((*picked, history_key || hits[*picked].1));
                            }
                        }
                    }
                    ui.add_space(8.0);
                    let chosen = hits.get(*picked).cloned();
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                keep = false;
                            }
                            let has = chosen.is_some();
                            if ui.add_enabled(has, egui::Button::new("History")).on_hover_text("Ctrl/Cmd+Enter").clicked() {
                                open = Some((*picked, true));
                            }
                            let can_blame = chosen.as_ref().is_some_and(|c| !c.1);
                            if ui.add_enabled(can_blame, egui::Button::new(RichText::new("Blame").strong()).fill(ACCENT_DIM)).on_hover_text("Enter").clicked() {
                                open = Some((*picked, false));
                            }
                        });
                    });
                    if let Some((i, history)) = open
                        && let Some((path, folder)) = hits.get(i).cloned()
                    {
                        actions.push(Action::File(if history || folder {
                            FileOpen::History { path, folder }
                        } else {
                            FileOpen::Blame { rev: None, path, line: None, back: false }
                        }));
                        keep = false;
                    }
                }
                Dialog::Confirm { title, body, button, danger, then } => {
                    ui.heading(title.as_str());
                    ui.add_space(4.0);
                    ui.label(body.as_str());
                    ui.add_space(10.0);
                    let (ok, cancel) = dialog_buttons(ui, button, true, *danger);
                    if ok {
                        if let Some(a) = then.take() {
                            actions.push(*a);
                        }
                        keep = false;
                    }
                    if cancel {
                        keep = false;
                    }
                }
            }
        });
        if modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            keep = false;
        }
        // Apply results that need the dialog's own data.
        if let Some(name) = self.pending_name.take() {
            match &dialog {
                Dialog::NewWorkspace { .. } => {
                    self.cfg.workspaces.push(Workspace::new(&name));
                    actions.push(Action::SwitchWorkspace(self.cfg.workspaces.len() - 1));
                }
                _ => self.cfg.ws_mut().name = name,
            }
            self.save();
        }
        if let Some(p) = self.pending_add.take() {
            self.scan_and_add(p);
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    fn take_screenshot_if_due(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        if shot.done.load(Ordering::SeqCst) {
            if let Some(e) = shot.error.lock().ok().and_then(|mut e| e.take()) {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Wait for the first data to arrive, but never longer than ~10 s.
        let settled = !self.samples_pending
            && self.busy.is_empty()
            && self.tabs.values().all(|t| !t.loading && !t.detail_loading)
            && self.summaries.len() >= self.cfg.ws().repos.len();
        if !shot.requested && ((self.frame_no >= 120 && settled) || self.frame_no >= 600) {
            shot.requested = true;
            let path = shot.path.clone();
            let done = shot.done.clone();
            let error = shot.error.clone();
            let ctx2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                let res = image::RgbaImage::from_raw(w as u32, h as u32, bytes)
                    .ok_or_else(|| "bad image size".to_string())
                    .and_then(|im| {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        im.save(&path).map_err(|e| e.to_string())
                    });
                if let Err(e) = res {
                    *error.lock().unwrap() = Some(e);
                }
                done.store(true, Ordering::SeqCst);
                ctx2.request_repaint();
            });
        }
    }
}

impl eframe::App for HeadwayApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame_no += 1;
        self.handle_messages();

        // Refresh when the window comes back into focus (files may have changed elsewhere).
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.focused {
            if let Some(p) = self.active_path() {
                self.reload_repo(&p);
            }
            self.refresh_overview();
        }
        self.focused = focused;

        let mut actions = Vec::new();

        // Keyboard shortcuts
        ctx.input(|i| {
            if i.modifiers.command && i.key_pressed(egui::Key::R) {
                actions.push(Action::Refresh);
            }
        });
        // Ctrl/Cmd+Z undoes the open repository's last action (text boxes keep their own undo).
        if self.dialog.is_none() && !ctx.text_edit_focused() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)) {
            if let Some(p) = self.active_path() {
                match self.tabs.get(&p).and_then(|rv| rv.data.as_ref()).and_then(|d| d.undo.as_ref()) {
                    Some(e) => actions.push(Action::Undo { repo: p, id: Some(e.id) }),
                    None => self.toast("Nothing to undo", false),
                }
            }
        }

        // Ctrl/Cmd+P finds a file in the open repository, to blame it or see its history.
        if self.dialog.is_none() && self.active_path().is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)) {
            actions.push(Action::FindFile);
        }

        egui::Panel::top("workspace_bar")
            .exact_size(40.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.workspace_bar(ui, &mut actions));

        egui::Panel::bottom("status_bar")
            .exact_size(24.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| self.status_bar(ui));

        egui::CentralPanel::default().frame(egui::Frame::new().fill(PANEL)).show(ui, |ui| {
            match self.active_path() {
                None => {
                    let ws = self.cfg.ws().clone();
                    overview::show(ui, &ws, &self.summaries, &self.busy, &mut actions);
                }
                Some(p) => {
                    let ws = self.cfg.ws().clone();
                    let busy: Vec<String> = self.busy.iter().filter(|b| b.1 == p).map(|b| b.2.clone()).collect();
                    if let Some(rv) = self.tabs.get_mut(&p) {
                        rv.modal = self.dialog.is_some();
                        let before = self.cfg.view;
                        views::repo(ui, rv, &ws, &self.summaries, &busy, &mut self.cfg.view, &mut actions);
                        if self.cfg.view != before {
                            self.view_dirty = true;
                        }
                    }
                }
            }
        });

        self.dialog_ui(&ctx, &mut actions);
        self.toasts_ui(&ctx, &mut actions);

        for a in actions {
            self.do_action(a);
        }
        if self.view_dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.view_dirty = false;
            self.cfg.save();
        }
        if !self.busy.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.take_screenshot_if_due(&ctx);
    }
}

// ---------------------------------------------------------------------------
// Small shared widgets

fn tab_button(ui: &mut Ui, text: &str, active: bool, dirty: Option<bool>) -> (egui::Response, bool) {
    let font = theme::prop(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, if active { TEXT } else { DIM });
    let close_w = if dirty.is_some() { 20.0 } else { 0.0 };
    let size = egui::vec2(galley.size().x + 26.0 + close_w, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, text));
    let hovered = resp.hovered();
    let fill = if active { PANEL } else if hovered { HOVER } else { BG };
    ui.painter().rect_filled(rect, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, fill);
    if active {
        let line = egui::Rect::from_min_size(rect.left_top(), egui::vec2(rect.width(), 2.0));
        ui.painter().rect_filled(line, 0.0, ACCENT);
    }
    let text_pos = egui::pos2(rect.left() + 13.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, TEXT);
    let mut close = false;
    if let Some(dirty) = dirty {
        let c = egui::pos2(rect.right() - 16.0, rect.center().y);
        let cr = egui::Rect::from_center_size(c, egui::vec2(16.0, 16.0));
        let cresp = ui.interact(cr, resp.id.with("close"), Sense::click());
        cresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Close {text}")));
        if cresp.hovered() {
            ui.painter().rect_filled(cr, 3.0, RAISED);
        }
        if cresp.hovered() || hovered || !dirty {
            ui.painter().text(c, egui::Align2::CENTER_CENTER, "×", theme::prop(14.0), if cresp.hovered() { TEXT } else { FAINT });
        } else {
            ui.painter().circle_filled(c, 3.5, WARN);
        }
        close = cresp.clicked();
    }
    (resp, close)
}

pub fn add_menu(ui: &mut Ui, label: &str, actions: &mut Vec<Action>) {
    ui.menu_button(RichText::new(label).strong(), |ui| {
        ui.set_min_width(240.0);
        if ui.button("Add repository…").clicked() {
            actions.push(Action::PickRepo);
            ui.close();
        }
        if ui.button("Add every repository in a folder…").clicked() {
            actions.push(Action::PickFolder);
            ui.close();
        }
        if ui.button("Type a folder path…").clicked() {
            actions.push(Action::Dialog(Dialog::AddPath { path: String::new() }));
            ui.close();
        }
        ui.separator();
        if ui.button("Clone from a web address…").clicked() {
            let dest = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
            actions.push(Action::Dialog(Dialog::Clone { url: String::new(), dest }));
            ui.close();
        }
    });
}

/// A one-field naming dialog. Returns true when confirmed.
fn name_dialog(ui: &mut Ui, title: &str, name: &mut String, enter: bool, keep: &mut bool) -> bool {
    ui.heading(title);
    ui.label(RichText::new("A workspace is a named group of repositories you work on together.").color(DIM));
    ui.add_space(8.0);
    ui.add(egui::TextEdit::singleline(name).hint_text("e.g. Client project").desired_width(f32::INFINITY))
        .request_focus();
    ui.add_space(8.0);
    let valid = !name.trim().is_empty();
    let (ok, cancel) = dialog_buttons(ui, "Save", valid, false);
    if cancel || (valid && (ok || enter)) {
        *keep = false;
    }
    valid && (ok || enter)
}

fn dialog_buttons(ui: &mut Ui, ok_label: &str, enabled: bool, danger: bool) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let fill = if danger { Color32::from_rgb(150, 55, 55) } else { ACCENT_DIM };
        ok = ui.add_enabled(enabled, egui::Button::new(RichText::new(ok_label).strong()).fill(fill)).clicked();
        cancel = ui.button("Cancel").clicked();
    });
    (ok, cancel)
}

fn valid_ref_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && !n.contains(char::is_whitespace)
        && !n.contains("..")
        && !n.starts_with('-')
        && !n.ends_with('/')
        && !n.ends_with(".lock")
        && !n.contains(['~', '^', ':', '?', '*', '[', '\\'])
}

fn first_file(st: &Status) -> Option<FileSel> {
    if let Some(f) = st.unstaged.first() {
        return Some(FileSel { path: f.path.clone(), staged: false, untracked: f.change == Change::Untracked });
    }
    st.staged.first().map(|f| FileSel { path: f.path.clone(), staged: true, untracked: false })
}

/// Turn common git errors into plain language.
pub fn friendly_error(e: &str) -> String {
    let lower = e.to_lowercase();
    let hint = if lower.contains("could not read username") || lower.contains("authentication failed") || lower.contains("terminal prompts disabled") {
        "The server asked for a login. Set up a credential helper or SSH key for this remote, then try again."
    } else if lower.contains("not possible to fast-forward") || lower.contains("diverged") {
        "Your branch and the remote branch have both changed. Merge or rebase them first."
    } else if lower.contains("would be overwritten") {
        "You have uncommitted changes that would be lost. Commit or stash them first."
    } else if lower.contains("not fully merged") {
        "This branch has commits that aren't merged anywhere. Use “Force delete” if you're sure."
    } else if lower.contains("conflict") {
        "There are conflicts. Fix the conflicted files, stage them, then commit."
    } else if lower.contains("no upstream") || lower.contains("has no upstream") {
        "This branch isn't linked to a remote branch yet. Push it first."
    } else {
        ""
    };
    let short: String = e.lines().filter(|l| !l.trim().is_empty()).take(4).collect::<Vec<_>>().join("\n");
    if hint.is_empty() { short } else { format!("{hint}\n\n{short}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("headway-apptest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Ana Lima"],
            &["config", "user.email", "ana@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git::run(&dir, args).unwrap();
        }
        dir
    }

    fn commit(dir: &Path, file: &str, msg: &str) {
        std::fs::write(dir.join(file), msg).unwrap();
        git::stage(dir, &[file.into()]).unwrap();
        git::commit(dir, msg, false).unwrap();
    }

    #[test]
    fn diff_doc_flattens_files_into_rows() {
        let files = git::parse_patch(
            "diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-x
+y
diff --git a/img b/img
Binary files a/img and b/img differ
",
        );
        let d = DiffDoc::new(files, true);
        assert!(matches!(
            d.rows[..],
            [
                FlatRow::File(0),
                FlatRow::Hunk(0, 0),
                FlatRow::Line(0, 0, 0),
                FlatRow::Line(0, 0, 1),
                FlatRow::Gap,
                FlatRow::File(1),
                FlatRow::Note(1),
                FlatRow::Gap
            ]
        ));
        assert_eq!(d.file_rows, [0, 5]);
        // An image file with a preview shows its picture rows instead of the note.
        let mut d = DiffDoc::new(git::parse_patch("diff --git a/i.png b/i.png\nnew file mode 100644\nBinary files /dev/null and b/i.png differ\n"), true);
        let png = {
            let mut out = std::io::Cursor::new(Vec::new());
            image::RgbaImage::new(8, 8).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let dir = temp_repo("preview");
        std::fs::write(dir.join("i.png"), &png).unwrap();
        d.modes = vec![views::DiffMode::Untracked];
        d.load_previews(&dir, None);
        let n = d.preview(0).unwrap().rows();
        assert!(n > 2);
        assert!(matches!(d.rows[0], FlatRow::File(0)));
        assert!(d.rows[1..=n].iter().enumerate().all(|(k, r)| matches!(r, FlatRow::Image(0, j) if *j == k)));
        assert!(matches!(d.rows[n + 1], FlatRow::Gap));
        d.toggle(0);
        assert!(matches!(d.rows[..], [FlatRow::File(0), FlatRow::Gap]), "folding hides the picture");
        let _ = std::fs::remove_dir_all(&dir);
        // Without file headers (single-file view) the hunk comes first.
        let d = DiffDoc::new(git::parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n"), false);
        assert!(matches!(d.rows[0], FlatRow::Hunk(0, 0)));
    }

    #[test]
    fn diffs_mark_the_changed_words_of_facing_lines() {
        let d = DiffDoc::new(git::parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n x\n"), false);
        let word = [8..9, 0..0];
        assert_eq!(d.marks(0, 0, 0), Some((&word[..1], false)));
        assert_eq!(d.marks(0, 0, 1), Some((&word[..1], false)));
        assert_eq!(d.marks(0, 0, 2), None, "unchanged lines have no marks");
        let mut d = d;
        assert!(!d.editable(0));
        d.modes = vec![views::DiffMode::Unstaged];
        assert!(d.editable(0));
        d.whitespace_ignored = true;
        assert!(!d.editable(0), "a -w diff can't be staged from");
    }

    fn two_file_doc() -> DiffDoc {
        DiffDoc::new(
            git::parse_patch(
                "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1,2 @@\n z\n+w\n",
            ),
            true,
        )
    }

    #[test]
    fn files_fold_down_to_their_name() {
        let mut d = two_file_doc();
        assert_eq!(d.rows.len(), 10);
        d.toggle(0);
        assert!(d.is_collapsed(0));
        assert!(matches!(d.rows[..2], [FlatRow::File(0), FlatRow::Gap]), "only the name bar is left");
        assert_eq!(d.file_rows, [0, 2], "the next file moves up");
        assert_eq!(d.rows.len(), 7);
        d.toggle(0);
        assert_eq!(d.rows.len(), 10);
        d.set_all_collapsed(true);
        assert_eq!(d.rows.len(), 4);
        d.set_collapsed(1, false);
        assert!(d.is_collapsed(0) && !d.is_collapsed(1));
        d.set_all_collapsed(false);
        assert!(d.collapsed.is_empty());
        // Single-file views (no name bar) never fold away.
        let mut single = DiffDoc::new(git::parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\n"), false);
        single.toggle(0);
        assert_eq!(single.rows.len(), 3);
    }

    #[test]
    fn uncommitted_changes_stack_in_groups_and_big_files_start_folded() {
        let small = |name: &str| git::parse_patch(&format!("diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1 +1 @@\n-x\n+y\n")).remove(0);
        let big_body: String = (0..400).map(|i| format!("+line {i}\n")).collect();
        let big = git::parse_patch(&format!("diff --git a/big b/big\n--- a/big\n+++ b/big\n@@ -0,0 +1,400 @@\n{big_body}")).remove(0);
        let mut new_file = small("new");
        new_file.change = Change::Untracked;
        let d = DiffDoc::working(vec![
            ("UNSTAGED".into(), views::DiffMode::Unstaged, vec![(small("a"), false), (big, false), (new_file, false), (small("sub"), true)]),
            ("STAGED".into(), views::DiffMode::Staged, vec![(small("s"), false)]),
            ("EMPTY".into(), views::DiffMode::Staged, vec![]),
        ]);
        assert_eq!(d.sections, [(0, "UNSTAGED".to_string()), (4, "STAGED".to_string())], "empty groups are left out");
        assert!(matches!(d.rows[0], FlatRow::Section(0)));
        assert!(d.is_collapsed(1) && !d.is_collapsed(0), "only the big file starts folded");
        assert_eq!(d.modes[2], views::DiffMode::Untracked, "new files are told apart from edits");
        assert_eq!(d.modes[4], views::DiffMode::Staged);
        assert!(d.editable(0) && !d.editable(3), "submodules get no line/block staging");
        assert_ne!(d.fold_key(0), d.fold_key(4));
        // The staged group's title comes right before its first file.
        let r = d.file_rows[4];
        assert!(matches!(d.rows[r - 1], FlatRow::Section(1)));
    }

    #[test]
    fn clicking_a_file_name_folds_and_unfolds_it() {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut doc = two_file_doc();
        let mut time = 0.0;
        // One frame of the diff viewer; returns the file whose name bar was clicked.
        let mut frame = |doc: &DiffDoc, events: Vec<egui::Event>| {
            time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let mut toggled = None;
            let mut out = ctx.run_ui(input, |ui| {
                let mut actions = Vec::new();
                toggled = views::diff_view(ui, doc, views::DiffMode::Commit, &mut None, None, Path::new("/r"), &mut actions);
            });
            out.textures_delta.clear();
            toggled
        };
        let click = |pos| {
            let b = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            vec![egui::Event::PointerMoved(pos), b(true), b(false)]
        };
        frame(&doc, vec![]);
        // The first row is file "a"'s name bar.
        let name_bar = egui::pos2(200.0, 8.0);
        let hit = frame(&doc, click(name_bar));
        assert_eq!(hit, Some(0));
        doc.toggle(0);
        frame(&doc, vec![]);
        assert!(doc.is_collapsed(0));
        // Clicking it again unfolds it.
        assert_eq!(frame(&doc, click(name_bar)), Some(0));
        doc.toggle(0);
        assert!(!doc.is_collapsed(0));
        // A click on a changed line doesn't fold anything.
        frame(&doc, vec![]);
        assert_eq!(frame(&doc, click(egui::pos2(200.0, 50.0))), None);
    }

    #[test]
    fn diff_doc_width_is_capped_for_huge_lines() {
        let long = "z".repeat(200_000);
        let d = DiffDoc::new(git::parse_patch(&format!("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -0,0 +1 @@\n+{long}\n")), false);
        assert!(d.max_cols <= views::MAX_LINE_CHARS + 24);
    }

    #[test]
    fn commits_on_the_branch_and_already_pushed() {
        let c = |h: &str, ps: &[&str], remote: bool| Commit {
            hash: h.into(),
            parents: ps.iter().map(|p| p.to_string()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            summary: String::new(),
            refs: if remote { vec![git::RefLabel { name: "origin/main".into(), kind: git::RefKind::Remote }] } else { vec![] },
        };
        // head → m (merge of b and s) → a; "o" is another branch; origin/main is at b.
        let commits = vec![c("head", &["m"], false), c("o", &["a"], false), c("m", &["b", "s"], false), c("s", &["a"], false), c("b", &["a"], true), c("a", &[], false)];
        let on = reachable(&commits, ["head"].into_iter());
        let mut on: Vec<_> = on.into_iter().collect();
        on.sort();
        assert_eq!(on, ["a", "b", "head", "m", "s"], "the other branch's commit isn't editable");
        let pushed = reachable(&commits, ["b"].into_iter());
        let mut pushed: Vec<_> = pushed.into_iter().collect();
        pushed.sort();
        assert_eq!(pushed, ["a", "b"]);
        assert!(reachable(&commits, ["unknown"].into_iter()).is_empty());
    }

    #[test]
    fn branch_and_tag_names_are_checked() {
        for ok in ["main", "feature/login", "v1.2.0", "fix-123", "  padded  "] {
            assert!(valid_ref_name(ok), "{ok}");
        }
        for bad in ["", "   ", "has space", "a..b", "-start", "end/", "x.lock", "a~1", "a^", "a:b", "what?", "st*r", "[x", "back\\slash"] {
            assert!(!valid_ref_name(bad), "{bad}");
        }
    }

    #[test]
    fn git_errors_get_a_plain_explanation() {
        let e = friendly_error("fatal: could not read Username for 'https://example.com': terminal prompts disabled");
        assert!(e.starts_with("The server asked for a login"));
        assert!(e.contains("could not read Username"));
        assert!(friendly_error("error: Your local changes to the following files would be overwritten by checkout").contains("Commit or stash them first"));
        assert!(friendly_error("error: The branch 'x' is not fully merged.").contains("Force delete"));
        // Unknown errors pass through, trimmed to a few lines.
        let long = (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(friendly_error(&long), "line 1\nline 2\nline 3\nline 4");
    }

    #[test]
    fn first_file_prefers_unstaged_changes() {
        let fc = |p: &str, c: Change| git::FileChange { path: p.into(), old_path: None, change: c, submodule: false };
        let mut st = Status::default();
        assert!(first_file(&st).is_none());
        st.staged.push(fc("s.txt", Change::Modified));
        assert_eq!(first_file(&st), Some(FileSel { path: "s.txt".into(), staged: true, untracked: false }));
        st.unstaged.push(fc("new.txt", Change::Untracked));
        assert_eq!(first_file(&st), Some(FileSel { path: "new.txt".into(), staged: false, untracked: true }));
    }

    #[test]
    fn loading_a_repository_and_searching_its_history() {
        let dir = temp_repo("load");
        commit(&dir, "a.txt", "Add login page");
        commit(&dir, "b.txt", "Fix crash on startup");
        git::run(&dir, &["tag", "release-1"]).unwrap();
        commit(&dir, "c.txt", "Polish login colours");
        std::fs::write(dir.join("wip.txt"), "draft").unwrap();

        let data = load_repo(&dir, PAGE, vec![]).unwrap();
        assert_eq!(data.commits.len(), 3);
        assert_eq!(data.graph.rows.len(), 3);
        assert_eq!(data.status.change_count(), 1);
        assert_eq!(data.tags, ["release-1"]);
        let fix_hash = data.commits[1].hash.clone();

        let mut rv = RepoView::new(dir.clone());
        rv.data = Some(data);
        assert_eq!(rv.head_branch(), Some("main"));
        let search = |rv: &mut RepoView, q: &str| {
            rv.search = q.into();
            rv.refilter();
            rv.filtered.clone()
        };
        assert_eq!(search(&mut rv, ""), [0, 1, 2]);
        assert_eq!(search(&mut rv, "LOGIN"), [0, 2], "search ignores case");
        assert_eq!(search(&mut rv, "ana lima").len(), 3, "search by author");
        assert_eq!(search(&mut rv, &fix_hash[..7]), [1], "search by commit id");
        assert_eq!(search(&mut rv, "release"), [1], "search by tag");
        assert!(search(&mut rv, "nothing like this").is_empty());

        // A limit loads only the newest commits.
        assert_eq!(load_repo(&dir, 2, vec![]).unwrap().commits.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loading_reports_missing_or_plain_folders() {
        let missing = std::env::temp_dir().join("headway-apptest-does-not-exist");
        assert!(load_repo(&missing, PAGE, vec![]).err().unwrap().contains("no longer exists"));
        let plain = std::env::temp_dir().join(format!("headway-apptest-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        assert!(load_repo(&plain, PAGE, vec![]).err().unwrap().contains("not a git repository"));
        assert!(load_summary(&plain).is_err());
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn history_lists_each_commit_once_after_editing_an_old_message() {
        let dir = temp_repo("reword-view");
        commit(&dir, "a.txt", "Initial storefront skeleton");
        commit(&dir, "b.txt", "Add base stylesheet");
        git::run(&dir, &["checkout", "-q", "-b", "feature/checkout"]).unwrap();
        commit(&dir, "c.txt", "Start checkout flow");
        git::run(&dir, &["checkout", "-q", "main"]).unwrap();
        commit(&dir, "d.txt", "Refine typography");
        git::run(&dir, &["merge", "-q", "--no-ff", "--no-edit", "feature/checkout"]).unwrap();
        git::run(&dir, &["tag", "v1.0.0"]).unwrap();
        git::run(&dir, &["checkout", "-q", "-b", "feature/search"]).unwrap();
        commit(&dir, "e.txt", "Add product search");
        git::run(&dir, &["checkout", "-q", "main"]).unwrap();
        commit(&dir, "f.txt", "Count quantities");

        let before = load_repo(&dir, PAGE, vec![]).unwrap();
        let root = before.commits.iter().find(|c| c.summary == "Initial storefront skeleton").unwrap().hash.clone();
        assert!(before.on_branch.contains(&root));
        let new = git::reword(&dir, &root, "Initial storefront").unwrap();

        let after = load_repo(&dir, PAGE, vec![]).unwrap();
        assert_eq!(after.commits.len(), before.commits.len(), "no duplicates in the history");
        let mut subjects: Vec<&str> = after.commits.iter().map(|c| c.summary.as_str()).collect();
        subjects.sort();
        subjects.dedup();
        assert_eq!(subjects.len(), after.commits.len());
        assert!(after.commits.iter().any(|c| c.hash == new && c.summary == "Initial storefront"));
        assert!(after.commits.iter().all(|c| c.summary != "Initial storefront skeleton"));
        // Every branch and the tag still show up, on the new commits.
        for name in ["main", "feature/checkout", "feature/search", "v1.0.0"] {
            assert!(after.commits.iter().any(|c| c.refs.iter().any(|r| r.name == name)), "{name}");
        }
        // One connected history again: a single root.
        assert_eq!(after.commits.iter().filter(|c| c.parents.is_empty()).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_repository_loads_without_commits() {
        let dir = temp_repo("empty");
        let data = load_repo(&dir, PAGE, vec![]).unwrap();
        assert!(data.commits.is_empty());
        assert_eq!(data.status.head.branch.as_deref(), Some("main"));
        let s = load_summary(&dir).unwrap();
        assert!(s.last.is_none() && !s.has_remote);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_shows_latest_commit() {
        let dir = temp_repo("summary");
        commit(&dir, "a.txt", "First");
        commit(&dir, "b.txt", "Second thing");
        let s = load_summary(&dir).unwrap();
        let last = s.last.unwrap();
        assert_eq!(last.summary, "Second thing");
        assert_eq!(last.author, "Ana Lima");
        assert_eq!(s.status.change_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;
