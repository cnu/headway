//! Thin, fast wrapper around the `git` command line.
//!
//! Everything here is blocking and is meant to be called from worker threads.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

pub type GitResult<T> = Result<T, String>;

fn base_cmd(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        // Personal git settings must not change the output we read.
        .args([
            "-c", "color.ui=false",
            "-c", "color.diff=false",
            "-c", "color.status=false",
            "-c", "color.branch=false",
            "-c", "core.quotepath=false",
            "-c", "diff.mnemonicPrefix=false",
            "-c", "diff.noprefix=false",
            "-c", "diff.relative=false",
            "-c", "log.showSignature=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run git and return stdout. Non-zero exit becomes an error with stderr.
pub fn run(repo: &Path, args: &[&str]) -> GitResult<String> {
    let out = base_cmd(repo)
        .args(args)
        .output()
        .map_err(|e| format!("Could not start git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        };
        Err(if msg.is_empty() { format!("git {} failed", args.join(" ")) } else { msg })
    }
}

/// Run git, accepting any exit code (used for `diff --no-index` which exits 1 on differences).
fn run_any(repo: &Path, args: &[&str]) -> GitResult<String> {
    let out = base_cmd(repo)
        .args(args)
        .output()
        .map_err(|e| format!("Could not start git: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run git with data on stdin.
pub fn run_stdin(repo: &Path, args: &[&str], input: &str) -> GitResult<String> {
    run_stdin_env(repo, args, input, &[])
}

/// Like [`run_stdin`], with extra environment variables for git.
fn run_stdin_env(repo: &Path, args: &[&str], input: &str, env: &[(&str, &str)]) -> GitResult<String> {
    let mut child = base_cmd(repo)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start git: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn is_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

pub fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------
// Data types

#[derive(Clone, Debug, PartialEq)]
pub enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub summary: String,
    pub refs: Vec<RefLabel>,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(8)]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Head {
    pub branch: Option<String>,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    Conflict(ConflictKind),
}

impl Change {
    pub fn letter(&self) -> &'static str {
        match self {
            Change::Modified => "M",
            Change::Added => "A",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::Copied => "C",
            Change::TypeChange => "T",
            Change::Untracked => "?",
            Change::Conflict(_) => "!",
        }
    }

    fn from_code(c: char) -> Option<Change> {
        Some(match c {
            'M' => Change::Modified,
            'A' => Change::Added,
            'D' => Change::Deleted,
            'R' => Change::Renamed,
            'C' => Change::Copied,
            'T' => Change::TypeChange,
            _ => return None,
        })
    }
}

/// How the two sides of a merge disagree about a file (the status code git reports).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// UU: both sides changed it.
    BothModified,
    /// AA: both sides added a file with this name.
    BothAdded,
    /// DU: our side deleted it, their side changed it.
    DeletedByUs,
    /// UD: their side deleted it, our side changed it.
    DeletedByThem,
    /// AU: only our side has it (usually a rename on one side).
    AddedByUs,
    /// UA: only their side has it.
    AddedByThem,
    /// DD: both sides deleted (or renamed) it.
    BothDeleted,
}

impl ConflictKind {
    fn from_code(xy: &str) -> ConflictKind {
        match xy {
            "AA" => ConflictKind::BothAdded,
            "DU" => ConflictKind::DeletedByUs,
            "UD" => ConflictKind::DeletedByThem,
            "AU" => ConflictKind::AddedByUs,
            "UA" => ConflictKind::AddedByThem,
            "DD" => ConflictKind::BothDeleted,
            _ => ConflictKind::BothModified,
        }
    }

    /// Both sides have the file, so its contents can be merged region by region.
    pub fn has_regions(self) -> bool {
        matches!(self, ConflictKind::BothModified | ConflictKind::BothAdded)
    }
}

#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub change: Change,
    /// The path is a submodule (its recorded commit or contents changed).
    pub submodule: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub head: Head,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub conflicts: Vec<FileChange>,
    /// Number of distinct changed files, counted once when read (it's asked for every frame).
    pub changes: usize,
}

impl Status {
    pub fn change_count(&self) -> usize {
        self.changes
    }

    fn count_changes(&self) -> usize {
        let mut paths: Vec<&str> = self
            .staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.conflicts)
            .map(|f| f.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths.len()
    }
}

#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub is_head: bool,
    pub remote: bool,
    pub upstream: Option<String>,
    pub track: String,
}

#[derive(Clone, Debug)]
pub struct Stash {
    pub index: usize,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Status

pub fn status(repo: &Path) -> GitResult<Status> {
    let out = run(
        repo,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
    )?;
    let mut st = Status::default();
    let mut parts = out.split('\0').peekable();
    while let Some(entry) = parts.next() {
        if entry.is_empty() {
            continue;
        }
        if let Some(rest) = entry.strip_prefix("# ") {
            let mut it = rest.splitn(2, ' ');
            let key = it.next().unwrap_or("");
            let val = it.next().unwrap_or("");
            match key {
                "branch.oid" if val != "(initial)" => st.head.oid = Some(val.to_string()),
                "branch.head" if val != "(detached)" => st.head.branch = Some(val.to_string()),
                "branch.upstream" => st.head.upstream = Some(val.to_string()),
                "branch.ab" => {
                    for tok in val.split(' ') {
                        if let Some(n) = tok.strip_prefix('+') {
                            st.head.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = tok.strip_prefix('-') {
                            st.head.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = entry.as_bytes()[0];
        match kind {
            b'1' | b'2' => {
                let n = if kind == b'1' { 9 } else { 10 };
                let fields: Vec<&str> = entry.splitn(n, ' ').collect();
                if fields.len() < n {
                    continue;
                }
                let xy: Vec<char> = fields[1].chars().collect();
                // "N..." for ordinary files, "S<c><m><u>" for submodules.
                let submodule = fields[2].starts_with('S');
                let path = fields[n - 1].to_string();
                let old_path = if kind == b'2' {
                    parts.next().map(|s| s.to_string())
                } else {
                    None
                };
                if let Some(c) = Change::from_code(xy[0]) {
                    st.staged.push(FileChange { path: path.clone(), old_path: old_path.clone(), change: c, submodule });
                }
                if let Some(c) = Change::from_code(xy[1]) {
                    st.unstaged.push(FileChange { path, old_path: None, change: c, submodule });
                }
            }
            b'u' => {
                let fields: Vec<&str> = entry.splitn(11, ' ').collect();
                if let (Some(xy), Some(p)) = (fields.get(1), fields.last()) {
                    let change = Change::Conflict(ConflictKind::from_code(xy));
                    st.conflicts.push(FileChange { path: p.to_string(), old_path: None, change, submodule: false });
                }
            }
            b'?' => {
                st.unstaged.push(FileChange {
                    path: entry[2..].to_string(),
                    old_path: None,
                    change: Change::Untracked,
                    submodule: false,
                });
            }
            _ => {}
        }
    }
    st.changes = st.count_changes();
    Ok(st)
}

// ---------------------------------------------------------------------------
// History

#[cfg(test)]
pub fn log(repo: &Path, limit: usize) -> GitResult<Vec<Commit>> {
    log_filtered(repo, limit, &[])
}

/// The full ref name behind a history label ("refs/heads/main", "refs/remotes/origin/x", …).
pub fn label_ref(r: &RefLabel) -> Option<String> {
    match r.kind {
        RefKind::Head if r.name == "HEAD" => None,
        RefKind::Head | RefKind::Local => Some(format!("refs/heads/{}", r.name)),
        RefKind::Remote => Some(format!("refs/remotes/{}", r.name)),
        RefKind::Tag => Some(format!("refs/tags/{}", r.name)),
    }
}

/// History of every branch except the `hidden` ones (full ref names). Commits that
/// only hidden branches reach are left out, and hidden branches lose their labels.
pub fn log_filtered(repo: &Path, limit: usize, hidden: &[String]) -> GitResult<Vec<Commit>> {
    let n = format!("-n{limit}");
    let excludes: Vec<String> = hidden.iter().map(|r| format!("--exclude={r}")).collect();
    let mut args: Vec<&str> = vec![
        "log",
        "--topo-order",
        &n,
        // Full names, so "fix/x" (a local branch) isn't mistaken for a remote one.
        "--decorate=full",
        "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s%x1f%D%x1e",
    ];
    // Each --exclude applies to the --all that follows it.
    args.extend(excludes.iter().map(|s| s.as_str()));
    args.extend(["--exclude=refs/stash", "--all"]);
    let out = match run(repo, &args) {
        Ok(o) => o,
        // A brand new repository without commits.
        Err(e) if e.contains("does not have any commits") || e.contains("bad default revision") => {
            String::new()
        }
        Err(e) => return Err(e),
    };
    let mut commits = Vec::new();
    for rec in out.split('\x1e') {
        let rec = rec.trim_start_matches('\n');
        if rec.is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.split('\x1f').collect();
        if f.len() < 7 {
            continue;
        }
        commits.push(Commit {
            hash: f[0].to_string(),
            parents: f[1].split(' ').filter(|s| !s.is_empty()).map(String::from).collect(),
            author: f[2].to_string(),
            email: f[3].to_string(),
            time: f[4].parse().unwrap_or(0),
            summary: f[5].to_string(),
            refs: parse_decorations(f[6])
                .into_iter()
                .filter(|r| !label_ref(r).is_some_and(|full| hidden.contains(&full)))
                .collect(),
        });
    }
    Ok(commits)
}

fn parse_decorations(d: &str) -> Vec<RefLabel> {
    let mut out = Vec::new();
    for part in d.split(", ").filter(|s| !s.is_empty()) {
        let (head, name) = match part.strip_prefix("HEAD -> ") {
            Some(rest) => (true, rest),
            None => (false, part),
        };
        let name = name.strip_prefix("tag: ").unwrap_or(name);
        let label = |n: &str, kind| RefLabel { name: n.to_string(), kind };
        if name == "HEAD" {
            out.push(label("HEAD", RefKind::Head));
        } else if let Some(b) = name.strip_prefix("refs/heads/") {
            out.push(label(b, if head { RefKind::Head } else { RefKind::Local }));
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            out.push(label(t, RefKind::Tag));
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            if !r.ends_with("/HEAD") {
                out.push(label(r, RefKind::Remote));
            }
        }
        // Anything else (notes, other ref namespaces) isn't shown.
    }
    out
}

pub fn branches(repo: &Path) -> GitResult<(Vec<Branch>, Vec<String>)> {
    let out = run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%1f%(HEAD)%1f%(upstream:short)%1f%(upstream:track,nobracket)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let mut branches = Vec::new();
    let mut tags = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split('\x1f').collect();
        if f.len() < 4 {
            continue;
        }
        if let Some(n) = f[0].strip_prefix("refs/heads/") {
            branches.push(Branch {
                name: n.to_string(),
                is_head: f[1] == "*",
                remote: false,
                upstream: (!f[2].is_empty()).then(|| f[2].to_string()),
                track: f[3].to_string(),
            });
        } else if let Some(n) = f[0].strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            branches.push(Branch {
                name: n.to_string(),
                is_head: false,
                remote: true,
                upstream: None,
                track: String::new(),
            });
        } else if let Some(n) = f[0].strip_prefix("refs/tags/") {
            tags.push(n.to_string());
        }
    }
    tags.reverse();
    Ok((branches, tags))
}

pub fn remotes(repo: &Path) -> GitResult<Vec<String>> {
    Ok(run(repo, &["remote"])?.lines().map(String::from).collect())
}

/// Each remote's fetch address, from `git remote -v`.
pub fn remote_urls(repo: &Path) -> GitResult<Vec<(String, String)>> {
    Ok(parse_remote_urls(&run(repo, &["remote", "-v"])?))
}

fn parse_remote_urls(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter(|l| l.ends_with("(fetch)"))
        .filter_map(|l| {
            let (name, rest) = l.split_once('\t')?;
            Some((name.to_string(), rest.trim_end_matches("(fetch)").trim().to_string()))
        })
        .collect()
}

/// A remote name git accepts and that won't be confused with a path.
pub fn valid_remote_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && !n.starts_with('-')
        && !n.starts_with('.')
        && !n.ends_with(".lock")
        && n.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Add a remote, then (optionally) fetch it. A failed fetch still leaves the remote added.
pub fn add_remote(repo: &Path, name: &str, url: &str, fetch: bool) -> GitResult<String> {
    run(repo, &["remote", "add", name.trim(), url.trim()])?;
    if fetch {
        run(repo, &["fetch", name.trim()]).map_err(|e| format!("The remote “{}” was added, but fetching it failed:\n{e}", name.trim()))?;
    }
    Ok(String::new())
}

pub fn remove_remote(repo: &Path, name: &str) -> GitResult<String> {
    run(repo, &["remote", "remove", name])
}

pub fn stashes(repo: &Path) -> GitResult<Vec<Stash>> {
    let out = run(repo, &["stash", "list", "--format=%gs"]).unwrap_or_default();
    Ok(out
        .lines()
        .enumerate()
        .map(|(index, m)| Stash { index, message: m.to_string() })
        .collect())
}

/// State of a submodule, from `git submodule status`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SubState {
    /// Checked out at the commit the parent repository records.
    Ready,
    /// Not cloned/checked out yet ("git submodule update --init" needed).
    NotSetUp,
    /// Checked out at a different commit than the recorded one.
    Moved,
    /// Has merge conflicts.
    Conflict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Submodule {
    pub name: String,
    /// Folder inside the parent repository.
    pub path: String,
    pub url: Option<String>,
    /// Commit currently checked out (or recorded, if not set up).
    pub sha: String,
    pub state: SubState,
}

/// Read `git submodule status` together with the names/addresses from `.gitmodules`.
pub fn parse_submodules(status: &str, config: &str) -> Vec<Submodule> {
    // "submodule.<name>.path value" / "submodule.<name>.url value"
    let mut by_path: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut urls: HashMap<String, String> = HashMap::new();
    for line in config.lines() {
        let Some((key, val)) = line.split_once(' ') else { continue };
        let Some(rest) = key.strip_prefix("submodule.") else { continue };
        if let Some(name) = rest.strip_suffix(".path") {
            by_path.insert(val.to_string(), (name.to_string(), None));
        } else if let Some(name) = rest.strip_suffix(".url") {
            urls.insert(name.to_string(), val.to_string());
        }
    }
    status
        .lines()
        .filter(|l| l.len() > 41)
        .map(|l| {
            let state = match l.as_bytes()[0] {
                b'-' => SubState::NotSetUp,
                b'+' => SubState::Moved,
                b'U' => SubState::Conflict,
                _ => SubState::Ready,
            };
            let rest = &l[1..];
            let (sha, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            // A trailing " (describe)" isn't part of the path.
            let path = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => &rest[..i],
                _ => rest,
            };
            let (name, _) = by_path.get(path).cloned().unwrap_or_else(|| (path.to_string(), None));
            Submodule { url: urls.get(&name).cloned(), name, path: path.to_string(), sha: sha.to_string(), state }
        })
        .collect()
}

pub fn submodules(repo: &Path) -> GitResult<Vec<Submodule>> {
    if !repo.join(".gitmodules").exists() {
        return Ok(Vec::new());
    }
    let status = run(repo, &["submodule", "status"])?;
    let config = run(repo, &["config", "-f", ".gitmodules", "--get-regexp", "^submodule\\."]).unwrap_or_default();
    Ok(parse_submodules(&status, &config))
}

/// A folder name for a new submodule, taken from its address.
pub fn submodule_folder(url: &str) -> String {
    url.trim().trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("").trim_end_matches(".git").to_string()
}

pub fn add_submodule(repo: &Path, url: &str, path: &str) -> GitResult<String> {
    run(repo, &["submodule", "add", "--", url.trim(), path.trim()])
}

/// Check out the recorded commit (or, with `latest`, the newest commit of the
/// submodule's own branch) — for one submodule or all of them.
pub fn update_submodules(repo: &Path, path: Option<&str>, latest: bool) -> GitResult<String> {
    let mut args = vec!["submodule", "update", "--init", "--recursive"];
    if latest {
        args.push("--remote");
    }
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    run(repo, &args)
}

/// Remove a submodule completely: its folder, its entry in `.gitmodules` and its stored clone.
pub fn remove_submodule(repo: &Path, sub: &Submodule) -> GitResult<String> {
    run(repo, &["submodule", "deinit", "-f", "--", &sub.path])?;
    run(repo, &["rm", "-f", "-q", "--", &sub.path])?;
    let modules = run(repo, &["rev-parse", "--git-path", &format!("modules/{}", sub.name)])?;
    let modules = repo.join(modules.trim());
    if modules.exists() {
        std::fs::remove_dir_all(&modules).map_err(|e| e.to_string())?;
    }
    Ok(String::new())
}

/// A working folder attached to the repository (`git worktree`).
#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    /// Branch checked out there, without `refs/heads/`.
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    /// Its folder has gone missing; `git worktree prune` would forget it.
    pub prunable: bool,
    /// The repository's original folder (listed first by git).
    pub is_main: bool,
    /// The folder being looked at right now.
    pub is_current: bool,
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Read `git worktree list --porcelain`.
pub fn parse_worktrees(out: &str, current: &Path) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            let path = PathBuf::from(p);
            list.push(Worktree {
                is_main: list.is_empty(),
                is_current: same_path(&path, current),
                path,
                head: String::new(),
                branch: None,
                bare: false,
                detached: false,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(w) = list.last_mut() else { continue };
        let (key, val) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "HEAD" => w.head = val.to_string(),
            "branch" => w.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string()),
            "bare" => w.bare = true,
            "detached" => w.detached = true,
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    list
}

pub fn worktrees(repo: &Path) -> GitResult<Vec<Worktree>> {
    Ok(parse_worktrees(&run(repo, &["worktree", "list", "--porcelain"])?, repo))
}

/// Create a worktree at `path`. With `new_from`, a new branch `branch` starts there;
/// otherwise the existing branch `branch` is checked out.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, new_from: Option<&str>) -> GitResult<String> {
    let p = path.to_string_lossy();
    match new_from {
        Some(start) => run(repo, &["worktree", "add", "-b", branch, &p, start]),
        None => run(repo, &["worktree", "add", &p, branch]),
    }
}

pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> GitResult<String> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    run(repo, &args)
}

/// A sensible folder for a new worktree: next to the repository, named after it and the branch.
pub fn default_worktree_path(repo: &Path, branch: &str) -> PathBuf {
    let leaf: String = branch
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect();
    let name = format!("{}-{}", repo_name(repo), leaf.trim_matches('-'));
    repo.parent().map(|p| p.join(&name)).unwrap_or_else(|| PathBuf::from(name))
}

pub struct CommitDetail {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub committer: String,
    pub time: i64,
    pub parents: Vec<String>,
    pub message: String,
    pub files: Vec<DiffFile>,
}

/// Said of a file whose only changes are to whitespace, while those are ignored.
pub const WHITESPACE_ONLY: &str = "Only whitespace changed (hidden by Ignore whitespace)";

/// `-w` when whitespace changes are ignored.
fn ws_args(ignore_whitespace: bool) -> &'static [&'static str] {
    if ignore_whitespace { &["-w"] } else { &[] }
}

/// With whitespace ignored, a file whose changes were all whitespace is left
/// with no hunks: say so instead of showing nothing.
fn note_whitespace_only(files: &mut [DiffFile], ignore_whitespace: bool) {
    if ignore_whitespace {
        for f in files.iter_mut().filter(|f| f.hunks.is_empty() && !f.binary && f.note.is_none() && f.header.contains("\nindex ")) {
            f.note = Some(WHITESPACE_ONLY.into());
        }
    }
}

/// `git show -w` leaves out files whose changes were all whitespace. Put them
/// back, in the commit's order, as notes saying so.
fn keep_whitespace_only_files(repo: &Path, hash: &str, paths: &[String], files: Vec<DiffFile>) -> GitResult<Vec<DiffFile>> {
    let mut args = vec!["show", "--format=", "--name-status", "-M", "--diff-merges=first-parent", hash, "--"];
    args.extend(paths.iter().map(String::as_str));
    let mut by_path: HashMap<String, DiffFile> = files.into_iter().map(|f| (f.path.clone(), f)).collect();
    let mut out = Vec::new();
    for line in run(repo, &args)?.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let Some(change) = f.first().and_then(|s| s.chars().next()).and_then(Change::from_code) else { continue };
        let (old, path) = match f.as_slice() {
            [_, old, new] => (Some(unquote_path(old)), unquote_path(new)),
            [_, path] => (None, unquote_path(path)),
            _ => continue,
        };
        out.push(by_path.remove(&path).unwrap_or_else(|| {
            DiffFile::placeholder(&FileChange { path: path.clone(), old_path: old, change, submodule: false }, WHITESPACE_ONLY)
        }));
    }
    // Anything name-status didn't list (it should list everything) goes at the end.
    out.extend(by_path.into_values());
    Ok(out)
}

pub fn commit_detail(repo: &Path, hash: &str, ignore_whitespace: bool) -> GitResult<CommitDetail> {
    let head = run(repo, &["show", "-s", "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%at%x1f%P%x1f%B", hash])?;
    let f: Vec<&str> = head.splitn(7, '\x1f').collect();
    if f.len() < 7 {
        return Err("Unexpected output from git show".into());
    }
    let mut args = vec!["show", "--no-ext-diff", "--full-index", "--format=", "--patch", "-M", "--diff-merges=first-parent"];
    args.extend(ws_args(ignore_whitespace));
    args.push(hash);
    let patch = run(repo, &args)?;
    let mut files = parse_patch(&patch);
    note_whitespace_only(&mut files, ignore_whitespace);
    if ignore_whitespace {
        files = keep_whitespace_only_files(repo, hash, &[], files)?;
    }
    Ok(CommitDetail {
        hash: f[0].to_string(),
        author: f[1].to_string(),
        email: f[2].to_string(),
        committer: f[3].to_string(),
        time: f[4].parse().unwrap_or(0),
        parents: f[5].split(' ').filter(|s| !s.is_empty()).map(String::from).collect(),
        message: f[6].trim_end().to_string(),
        files,
    })
}

// ---------------------------------------------------------------------------
// Diffs

#[derive(Clone, Debug, PartialEq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    NoNewline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub change: Change,
    /// The raw "diff --git ... +++" header, needed to build patches for hunk staging.
    pub header: String,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    pub added: u32,
    pub removed: u32,
    /// Shown instead of the contents (e.g. "too large to show").
    pub note: Option<String>,
    /// Blob ids of the two sides, from the `index` line (`None`: no such side,
    /// or not known). The working copy's id is the one git would give it.
    pub old_id: Option<String>,
    pub new_id: Option<String>,
}

impl DiffFile {
    /// An entry for a file whose contents aren't shown.
    pub fn placeholder(change: &FileChange, note: &str) -> Self {
        DiffFile {
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            change: change.change.clone(),
            header: String::new(),
            hunks: Vec::new(),
            binary: false,
            added: 0,
            removed: 0,
            note: Some(note.to_string()),
            old_id: None,
            new_id: None,
        }
    }

    /// Build a patch containing only one hunk of this file (for `git apply --cached`).
    pub fn hunk_patch(&self, i: usize) -> Option<String> {
        let h = self.hunks.get(i)?;
        let mut s = self.header.clone();
        s.push_str(&h.header);
        s.push('\n');
        for l in &h.lines {
            let p = match l.kind {
                LineKind::Context => ' ',
                LineKind::Add => '+',
                LineKind::Del => '-',
                LineKind::NoNewline => '\\',
            };
            s.push(p);
            s.push_str(&l.text);
            s.push('\n');
        }
        Some(s)
    }
}

fn unquote_path(p: &str) -> String {
    let p = p.trim();
    let p = p.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(p);
    p.to_string()
}

pub fn parse_patch(patch: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut in_header = false;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in patch.lines() {
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") {
            let rest = line.splitn(3, ' ').nth(2).unwrap_or("");
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, b)| b.to_string())
                .unwrap_or_else(|| rest.to_string());
            files.push(DiffFile {
                path: unquote_path(&path),
                old_path: None,
                change: Change::Modified,
                header: format!("{line}\n"),
                hunks: Vec::new(),
                binary: false,
                added: 0,
                removed: 0,
                note: None,
                old_id: None,
                new_id: None,
            });
            in_header = true;
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        if in_header {
            if line.starts_with("@@") {
                in_header = false;
            } else {
                file.header.push_str(line);
                file.header.push('\n');
                if line.starts_with("new file") {
                    file.change = Change::Added;
                } else if line.starts_with("deleted file") {
                    file.change = Change::Deleted;
                } else if let Some(p) = line.strip_prefix("rename from ") {
                    file.change = Change::Renamed;
                    file.old_path = Some(unquote_path(p));
                } else if let Some(p) = line.strip_prefix("rename to ") {
                    file.path = unquote_path(p);
                } else if let Some(p) = line.strip_prefix("+++ b/") {
                    file.path = unquote_path(p);
                } else if line.starts_with("Binary files") || line.starts_with("GIT binary patch") {
                    file.binary = true;
                } else if let Some((a, b)) = line.strip_prefix("index ").and_then(|r| r.split(' ').next()).and_then(|r| r.split_once("..")) {
                    // Combined diffs (merges) list several old ids: no single old side.
                    let id = |s: &str| (!s.contains(',') && !s.bytes().all(|c| c == b'0')).then(|| s.to_string());
                    file.old_id = id(a);
                    file.new_id = id(b);
                }
                continue;
            }
        }
        if line.starts_with("@@") {
            // @@ -a,b +c,d @@ context
            let mut nums = line.split(' ').skip(1);
            let o = nums.next().unwrap_or("-0");
            let n = nums.next().unwrap_or("+0");
            old_no = o.trim_start_matches('-').split(',').next().unwrap_or("0").parse().unwrap_or(0);
            new_no = n.trim_start_matches('+').split(',').next().unwrap_or("0").parse().unwrap_or(0);
            file.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
            continue;
        }
        let Some(hunk) = file.hunks.last_mut() else { continue };
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Add, &line[1..]),
            Some('-') => (LineKind::Del, &line[1..]),
            Some('\\') => (LineKind::NoNewline, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            None => (LineKind::Context, ""),
            _ => (LineKind::Context, line),
        };
        let (o, n) = match kind {
            LineKind::Add => {
                new_no += 1;
                file.added += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Del => {
                old_no += 1;
                file.removed += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            LineKind::NoNewline => (None, None),
        };
        hunk.lines.push(DiffLine { kind, old_no: o, new_no: n, text: text.to_string() });
    }
    files
}

/// Diff of one working-tree file. `staged` picks the index side.
pub fn file_diff(repo: &Path, path: &str, staged: bool, untracked: bool) -> GitResult<Option<DiffFile>> {
    let out = if untracked {
        run_any(repo, &["diff", "--no-ext-diff", "--full-index", "--no-index", "--", "/dev/null", path])?
    } else if staged {
        run(repo, &["diff", "--no-ext-diff", "--full-index", "--cached", "-M", "--", path])?
    } else {
        run(repo, &["diff", "--no-ext-diff", "--full-index", "--", path])?
    };
    let mut files = parse_patch(&out);
    if untracked {
        if let Some(f) = files.first_mut() {
            f.path = path.to_string();
            f.change = Change::Untracked;
        }
    }
    Ok(files.into_iter().next())
}

/// New files beyond this many aren't read for the stacked view (they're still listed).
pub const MAX_NEW_FILES_SHOWN: usize = 200;
/// New files larger than this aren't read either.
pub const MAX_NEW_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Diffs of every uncommitted change, in the order the status lists them:
/// (unstaged, including new files) and (staged).
/// With `ignore_whitespace`, changes to whitespace aren't shown (`-w`).
pub fn worktree_diffs(repo: &Path, st: &Status, ignore_whitespace: bool) -> GitResult<(Vec<DiffFile>, Vec<DiffFile>)> {
    let take = |args: &[&str]| -> GitResult<HashMap<String, DiffFile>> {
        let mut args = args.to_vec();
        args.extend(ws_args(ignore_whitespace));
        let mut files = parse_patch(&run(repo, &args)?);
        note_whitespace_only(&mut files, ignore_whitespace);
        Ok(files.into_iter().map(|f| (f.path.clone(), f)).collect())
    };
    let mut unstaged_map = take(&["diff", "--no-ext-diff", "--full-index"])?;
    let mut staged_map = take(&["diff", "--no-ext-diff", "--full-index", "--cached", "-M"])?;
    let none = if ignore_whitespace { WHITESPACE_ONLY } else { "No text changes" };
    let mut new_shown = 0;
    let unstaged = st
        .unstaged
        .iter()
        .map(|fc| {
            if fc.change != Change::Untracked {
                return unstaged_map.remove(&fc.path).unwrap_or_else(|| DiffFile::placeholder(fc, none));
            }
            let size = std::fs::metadata(repo.join(&fc.path)).map(|m| m.len()).unwrap_or(0);
            if size > MAX_NEW_FILE_BYTES {
                return DiffFile::placeholder(fc, "Large new file — contents not shown");
            }
            new_shown += 1;
            if new_shown > MAX_NEW_FILES_SHOWN {
                return DiffFile::placeholder(fc, "Many new files — contents not shown for this one");
            }
            match file_diff(repo, &fc.path, false, true) {
                Ok(Some(f)) => f,
                _ => DiffFile::placeholder(fc, "Empty new file"),
            }
        })
        .collect();
    let staged = st
        .staged
        .iter()
        .map(|fc| staged_map.remove(&fc.path).unwrap_or_else(|| DiffFile::placeholder(fc, none)))
        .collect();
    Ok((unstaged, staged))
}

// ---------------------------------------------------------------------------
// Actions

pub fn stage(repo: &Path, paths: &[String]) -> GitResult<String> {
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run(repo, &args)
}

pub fn unstage(repo: &Path, paths: &[String]) -> GitResult<String> {
    let has_head = run(repo, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
    let mut args: Vec<&str> = if has_head {
        vec!["reset", "-q", "HEAD", "--"]
    } else {
        vec!["rm", "--cached", "-q", "-r", "--"]
    };
    args.extend(paths.iter().map(|s| s.as_str()));
    run(repo, &args)
}

pub fn discard(repo: &Path, file: &FileChange) -> GitResult<String> {
    if file.submodule {
        // A submodule can't be "checked out" like a file: put it back on the recorded commit.
        return update_submodules(repo, Some(&file.path), false);
    }
    if file.change == Change::Untracked {
        std::fs::remove_file(repo.join(&file.path)).map_err(|e| e.to_string())?;
        Ok(String::new())
    } else {
        run(repo, &["checkout", "--", &file.path])
    }
}

pub fn apply_hunk(repo: &Path, patch: &str, reverse: bool, cached: bool) -> GitResult<String> {
    let mut args = vec!["apply", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.push("-");
    run_stdin(repo, &args, patch)
}

/// Which lines of a diff are chosen, as (hunk index, line index) pairs.
pub type LineSelection = std::collections::BTreeSet<(usize, usize)>;

impl DiffFile {
    /// Build a patch that contains only the chosen added/removed lines.
    ///
    /// `reverse` is for patches that will be applied with `-R` (unstaging, or discarding
    /// from the working copy): there the *new* side is what's on disk now, so unchosen
    /// added lines become context and unchosen removed lines are dropped. Applied forwards
    /// (staging) it's the mirror image.
    pub fn partial_patch(&self, sel: &LineSelection, reverse: bool) -> Option<String> {
        let mut body = String::new();
        let mut delta: i64 = 0;
        let mut chosen_total = 0usize;
        let mut changed_total = 0usize;
        for (hi, h) in self.hunks.iter().enumerate() {
            let (old_start, new_start) = parse_hunk_starts(&h.header);
            let mut lines = String::new();
            let (mut old_n, mut new_n) = (0u32, 0u32);
            let mut chosen = 0usize;
            let mut last_kept = false;
            for (li, l) in h.lines.iter().enumerate() {
                let picked = sel.contains(&(hi, li));
                let emit = match l.kind {
                    LineKind::Context => Some(' '),
                    LineKind::Add if picked => Some('+'),
                    LineKind::Del if picked => Some('-'),
                    LineKind::Add => reverse.then_some(' '),
                    LineKind::Del => (!reverse).then_some(' '),
                    LineKind::NoNewline => {
                        if last_kept {
                            lines.push_str("\\");
                            lines.push_str(&l.text);
                            lines.push('\n');
                        }
                        continue;
                    }
                };
                if matches!(l.kind, LineKind::Add | LineKind::Del) {
                    changed_total += 1;
                    if picked {
                        chosen += 1;
                    }
                }
                last_kept = emit.is_some();
                let Some(c) = emit else { continue };
                match c {
                    ' ' => {
                        old_n += 1;
                        new_n += 1;
                    }
                    '+' => new_n += 1,
                    _ => old_n += 1,
                }
                lines.push(c);
                lines.push_str(&l.text);
                lines.push('\n');
            }
            if chosen == 0 {
                continue;
            }
            chosen_total += chosen;
            // Keep the side git will look for in place; derive the other from the running offset.
            let (os, ns) = if reverse {
                ((new_start as i64 - delta).max(0), new_start as i64)
            } else {
                (old_start as i64, (old_start as i64 + delta).max(0))
            };
            let os = if old_n == 0 { os.max(0) } else { os.max(1) };
            let ns = if new_n == 0 { ns.max(0) } else { ns.max(1) };
            body.push_str(&format!("@@ -{os},{old_n} +{ns},{new_n} @@\n"));
            body.push_str(&lines);
            delta += new_n as i64 - old_n as i64;
        }
        if chosen_total == 0 {
            return None;
        }
        // A partial patch can't create or delete the whole file: turn such headers
        // into an ordinary "modified" header.
        let header = if chosen_total < changed_total {
            self.header
                .lines()
                .filter(|l| !l.starts_with("new file mode") && !l.starts_with("deleted file mode"))
                .map(|l| match l {
                    "--- /dev/null" => format!("--- a/{}", self.path),
                    "+++ /dev/null" => format!("+++ b/{}", self.path),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        } else {
            self.header.clone()
        };
        Some(header + &body)
    }
}

fn parse_hunk_starts(header: &str) -> (u32, u32) {
    let mut nums = header.split(' ').skip(1);
    let o = nums.next().unwrap_or("-0");
    let n = nums.next().unwrap_or("+0");
    let start = |s: &str| s[1..].split(',').next().unwrap_or("0").parse().unwrap_or(0);
    (start(o), start(n))
}

fn apply_patch(repo: &Path, patch: &str, reverse: bool, cached: bool) -> GitResult<String> {
    let mut args = vec!["apply", "--whitespace=nowarn", "--recount"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("-R");
    }
    args.push("-");
    run_stdin(repo, &args, patch)
}

/// Stage only the chosen lines of an unstaged (or new, untracked) file.
pub fn stage_lines(repo: &Path, file: &DiffFile, sel: &LineSelection, untracked: bool) -> GitResult<String> {
    if untracked {
        // Track the file with empty contents first, so the chosen lines can be added
        // like any other change.
        let blob = run_stdin(repo, &["hash-object", "-w", "--stdin"], "")?;
        let info = format!("100644,{},{}", blob.trim(), file.path);
        run(repo, &["update-index", "--add", "--cacheinfo", &info])?;
        let fresh = file_diff(repo, &file.path, false, false)?.ok_or("The file has no changes to stage")?;
        let patch = fresh.partial_patch(sel, false).ok_or("No changed lines are selected")?;
        return apply_patch(repo, &patch, false, true);
    }
    let patch = file.partial_patch(sel, false).ok_or("No changed lines are selected")?;
    apply_patch(repo, &patch, false, true)
}

/// Take the chosen lines of a staged file back out of the next commit.
pub fn unstage_lines(repo: &Path, file: &DiffFile, sel: &LineSelection) -> GitResult<String> {
    let patch = file.partial_patch(sel, true).ok_or("No changed lines are selected")?;
    apply_patch(repo, &patch, true, true)
}

/// Throw away the chosen lines' changes from the file on disk.
pub fn discard_lines(repo: &Path, file: &DiffFile, sel: &LineSelection) -> GitResult<String> {
    let patch = file.partial_patch(sel, true).ok_or("No changed lines are selected")?;
    apply_patch(repo, &patch, true, false)
}

pub fn commit(repo: &Path, message: &str, amend: bool) -> GitResult<String> {
    let mut args = vec!["commit", "-F", "-"];
    if amend {
        args.push("--amend");
    }
    run_stdin(repo, &args, message)
}

/// Change the message of `hash` (a commit on one of your branches or tags).
///
/// Only that commit's message changes, so every commit after it is copied exactly
/// (same files, author, committer and dates) with its parents pointing at the new
/// copies. Then every local branch, tag and a detached HEAD that pointed at an old
/// commit is moved to its copy, all at once. Files, the staging area and the
/// checked-out branch are untouched. Remote branches keep the old commits until
/// you force-push. Returns the edited commit's new id.
pub fn reword(repo: &Path, hash: &str, message: &str) -> GitResult<String> {
    let rev = |r: &str| run(repo, &["rev-parse", "--verify", "-q", r]).map(|s| s.trim().to_string());
    let target = rev(&format!("{hash}^{{commit}}")).map_err(|_| "That commit can't be found.".to_string())?;
    for state in ["rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        if run(repo, &["rev-parse", "--git-path", state]).is_ok_and(|p| repo.join(p.trim()).exists()) {
            return Err("A merge, rebase, cherry-pick or revert is in progress. Finish or abort it first.".into());
        }
    }

    // Local branches and tags, and HEAD when it isn't on a branch.
    struct Ref {
        name: String,
        oid: String,
        kind: String,
        peeled: String,
    }
    let refs: Vec<Ref> = run(repo, &["for-each-ref", "--format=%(refname)%1f%(objectname)%1f%(objecttype)%1f%(*objectname)", "refs/heads", "refs/tags"])?
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            (f.len() == 4).then(|| Ref { name: f[0].into(), oid: f[1].into(), kind: f[2].into(), peeled: f[3].into() })
        })
        .collect();
    let detached = run(repo, &["symbolic-ref", "-q", "HEAD"]).is_err();
    let head = rev("HEAD").ok();
    let mut tips: Vec<String> = refs.iter().map(|r| if r.kind == "tag" { r.peeled.clone() } else { r.oid.clone() }).filter(|t| !t.is_empty()).collect();
    if detached {
        tips.extend(head.clone());
    }
    tips.sort();
    tips.dedup();
    let on_a_ref = !run(repo, &["for-each-ref", "--contains", &target, "--format=%(refname)", "refs/heads", "refs/tags"])?.trim().is_empty();
    let on_head = detached && head.as_ref().is_some_and(|h| run(repo, &["merge-base", "--is-ancestor", &target, h]).is_ok());
    let reachable = on_a_ref || on_head;
    if !reachable {
        return Err("This commit isn't on any of your branches, so there's nothing to edit.".into());
    }

    // Every commit after the target (that a branch or tag can reach), parents first.
    let mut input: String = tips.iter().map(|t| format!("{t}\n")).collect();
    input.push_str(&format!("^{target}\n"));
    let list = run_stdin(repo, &["rev-list", "--topo-order", "--reverse", "--parents", "--ancestry-path", "--stdin"], &input)?;
    let order: Vec<(String, Vec<String>)> = list
        .lines()
        .filter_map(|l| {
            let mut it = l.split(' ').map(String::from);
            Some((it.next()?, it.collect()))
        })
        .collect();
    let mut wanted = vec![target.clone()];
    wanted.extend(order.iter().map(|(c, _)| c.clone()));
    let raw = read_objects(repo, &wanted)?;

    let mut map: HashMap<String, String> = HashMap::new();
    let first = raw.get(&target).ok_or("Couldn't read that commit.")?;
    map.insert(target.clone(), write_object(repo, "commit", &rewrite_commit(first, None, Some(message)))?);
    for (c, parents) in &order {
        if !parents.iter().any(|p| map.contains_key(p)) {
            continue;
        }
        let new_parents: Vec<String> = parents.iter().map(|p| map.get(p).cloned().unwrap_or_else(|| p.clone())).collect();
        let body = raw.get(c).ok_or("Couldn't read a later commit.")?;
        map.insert(c.clone(), write_object(repo, "commit", &rewrite_commit(body, Some(&new_parents), None))?);
    }

    // Move everything that pointed at an old commit, in one go.
    let mut tx = String::new();
    for r in &refs {
        if r.kind == "commit" {
            if let Some(n) = map.get(&r.oid) {
                tx.push_str(&format!("update {} {} {}\n", r.name, n, r.oid));
            }
        } else if r.kind == "tag" {
            if let Some(n) = map.get(&r.peeled) {
                let tag = run(repo, &["cat-file", "tag", &r.oid])?;
                if let Some(t) = rewrite_tag(&tag, n) {
                    tx.push_str(&format!("update {} {} {}\n", r.name, write_object(repo, "tag", &t)?, r.oid));
                }
            }
        }
    }
    if detached {
        if let Some(h) = &head {
            if let Some(n) = map.get(h) {
                tx.push_str(&format!("update HEAD {n} {h}\n"));
            }
        }
    }
    run_stdin(repo, &["update-ref", "-m", "Headway: edit commit message", "--no-deref", "--stdin"], &tx)?;
    Ok(map[&target].clone())
}

/// A commit's raw text with new parents and/or a new message. Signatures are dropped
/// (they'd no longer match); everything else is kept byte for byte.
fn rewrite_commit(raw: &str, parents: Option<&[String]>, message: Option<&str>) -> String {
    let (headers, body) = raw.split_once("\n\n").unwrap_or((raw, ""));
    let mut out = String::new();
    let mut in_signature = false;
    let mut parents_written = false;
    for line in headers.lines() {
        if in_signature && line.starts_with(' ') {
            continue;
        }
        in_signature = false;
        if line.starts_with("gpgsig ") || line.starts_with("gpgsig-sha256 ") {
            in_signature = true;
            continue;
        }
        if line.starts_with("parent ") {
            if let Some(ps) = parents {
                if !parents_written {
                    for p in ps {
                        out.push_str(&format!("parent {p}\n"));
                    }
                    parents_written = true;
                }
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    match message {
        Some(m) => {
            out.push_str(m.trim_end());
            out.push('\n');
        }
        None => out.push_str(body),
    }
    out
}

/// An annotated tag's raw text pointing at `new_commit`, without its (now invalid) signature.
fn rewrite_tag(raw: &str, new_commit: &str) -> Option<String> {
    let (headers, body) = raw.split_once("\n\n").unwrap_or((raw, ""));
    if !headers.lines().any(|l| l == "type commit") {
        return None; // a tag of a tag: leave it alone
    }
    let headers: Vec<String> = headers.lines().map(|l| if l.starts_with("object ") { format!("object {new_commit}") } else { l.to_string() }).collect();
    let body = ["-----BEGIN PGP SIGNATURE-----", "-----BEGIN SSH SIGNATURE-----", "-----BEGIN SIGNED MESSAGE-----"]
        .iter()
        .filter_map(|m| body.find(m))
        .min()
        .map(|i| &body[..i])
        .unwrap_or(body);
    Some(format!("{}\n\n{}", headers.join("\n"), body))
}

fn write_object(repo: &Path, kind: &str, text: &str) -> GitResult<String> {
    Ok(run_stdin(repo, &["hash-object", "-t", kind, "-w", "--stdin"], text)?.trim().to_string())
}

/// Read many objects with one `git cat-file --batch`.
fn read_objects(repo: &Path, ids: &[String]) -> GitResult<HashMap<String, String>> {
    let input: String = ids.iter().map(|i| format!("{i}\n")).collect();
    let mut child = base_cmd(repo).args(["cat-file", "--batch"]).stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    // Write on another thread so a large answer can't block us.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    let bytes = out.stdout;
    let mut map = HashMap::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some(nl) = bytes[i..].iter().position(|b| *b == b'\n') else { break };
        let header = String::from_utf8_lossy(&bytes[i..i + nl]).to_string();
        i += nl + 1;
        let f: Vec<&str> = header.split(' ').collect();
        if f.len() < 3 {
            return Err(format!("Couldn't read {}", f.first().unwrap_or(&"an object")));
        }
        let size: usize = f[2].parse().map_err(|_| "bad object size".to_string())?;
        let end = (i + size).min(bytes.len());
        map.insert(f[0].to_string(), String::from_utf8_lossy(&bytes[i..end]).into_owned());
        i = end + 1;
    }
    Ok(map)
}

/// Sizes of blobs in bytes, with one `git cat-file --batch-check`. Ids git
/// doesn't have are left out.
pub fn blob_sizes(repo: &Path, ids: &[String]) -> GitResult<HashMap<String, u64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let input: String = ids.iter().map(|i| format!("{i}\n")).collect();
    let out = run_stdin(repo, &["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"], &input)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f.len() == 3 && f[1] == "blob").then(|| Some((f[0].to_string(), f[2].parse().ok()?))).flatten()
        })
        .collect())
}

/// The text of blobs no bigger than `max` bytes (others, and ids git doesn't
/// have, are left out), with one `cat-file --batch-check` and one `--batch`.
pub fn small_blobs(repo: &Path, ids: &[String], max: u64) -> GitResult<HashMap<String, String>> {
    let wanted: Vec<String> = blob_sizes(repo, ids)?.into_iter().filter(|(_, n)| *n <= max).map(|(id, _)| id).collect();
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    read_objects(repo, &wanted)
}

pub fn last_message(repo: &Path) -> String {
    run(repo, &["log", "-1", "--format=%B"]).unwrap_or_default().trim_end().to_string()
}

pub fn clone(url: &str, dest_parent: &Path) -> GitResult<PathBuf> {
    let name = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_string();
    let dest = dest_parent.join(&name);
    run(dest_parent, &["clone", "--progress", "--recurse-submodules", url, &name])?;
    Ok(dest)
}

/// Find git repositories directly inside `dir` (and the folder itself).
pub fn scan_for_repos(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if is_repo(dir) {
        found.push(dir.to_path_buf());
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        entries.sort();
        for p in entries {
            if is_repo(&p) {
                found.push(p);
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Merges, rebases, cherry-picks and reverts in progress, and their conflicts

/// Which git command stopped part-way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpType {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl OpType {
    /// The git command, for `git <cmd> --continue` and friends.
    pub fn command(self) -> &'static str {
        match self {
            OpType::Merge => "merge",
            OpType::Rebase => "rebase",
            OpType::CherryPick => "cherry-pick",
            OpType::Revert => "revert",
        }
    }
}

/// A merge, rebase, cherry-pick or revert waiting to be continued or aborted.
#[derive(Clone, Debug, PartialEq)]
pub struct Operation {
    pub kind: OpType,
    /// The branch being changed (None when HEAD is detached).
    pub branch: Option<String>,
    /// Name for the "ours" side of each conflict: a branch name, or a short commit id.
    pub ours: String,
    /// Name for the "theirs" side.
    pub theirs: String,
    /// The commit being applied (rebase, cherry-pick, revert): "a3e4377 “Subject”".
    pub commit: Option<String>,
    /// Rebase: which commit of how many is being replayed.
    pub step: Option<(usize, usize)>,
    /// Cherry-pick and revert sequences: commits still waiting after this one.
    pub remaining: usize,
    /// Where HEAD was before it started (short id), for the Abort warning.
    pub before: Option<String>,
    /// The external merge tool from `merge.tool`, if one is set.
    pub merge_tool: Option<String>,
}

impl Operation {
    /// One line for the banner: "Merging feature/x into main".
    pub fn title(&self) -> String {
        let branch = self.branch.clone().unwrap_or_else(|| "detached HEAD".into());
        let commit = self.commit.clone().unwrap_or_else(|| self.theirs.clone());
        let mut s = match self.kind {
            OpType::Merge => format!("Merging {} into {branch}", self.theirs),
            OpType::Rebase => format!("Rebasing {branch} onto {}", self.ours),
            OpType::CherryPick => format!("Cherry-picking {commit}"),
            OpType::Revert => format!("Reverting {commit}"),
        };
        if let Some((n, of)) = self.step {
            s += &format!(" · commit {n} of {of}");
        }
        if self.remaining > 0 {
            s += &format!(" · {} more after this", self.remaining);
        }
        s
    }

    /// Skip only makes sense when there's a sequence of commits to carry on with.
    pub fn can_skip(&self) -> bool {
        self.kind == OpType::Rebase || self.remaining > 0
    }
}

/// Path of a file inside the repository's git folder (right for linked worktrees too).
fn git_path(repo: &Path, name: &str) -> GitResult<PathBuf> {
    Ok(repo.join(run(repo, &["rev-parse", "--git-path", name])?.trim_end_matches(['\n', '\r'])))
}

fn read_git_file(repo: &Path, name: &str) -> Option<String> {
    let p = git_path(repo, name).ok()?;
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// A commit's best name: a local branch pointing at it, else a remote branch, a tag, or its short id.
fn name_commit(repo: &Path, rev: &str) -> String {
    let refs = run(repo, &["for-each-ref", "--points-at", rev, "--format=%(refname)", "refs/heads", "refs/remotes", "refs/tags"]).unwrap_or_default();
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(n) = refs.lines().find_map(|r| r.strip_prefix(prefix)).filter(|n| !n.ends_with("/HEAD")) {
            return n.to_string();
        }
    }
    short_id(repo, rev)
}

fn short_id(repo: &Path, rev: &str) -> String {
    run(repo, &["rev-parse", "--short", rev]).map(|s| s.trim().to_string()).unwrap_or_else(|_| rev.chars().take(7).collect())
}

/// "a3e4377 “Subject”"
fn commit_title(repo: &Path, rev: &str) -> Option<String> {
    let out = run(repo, &["log", "-1", "--format=%h%x1f%s", rev]).ok()?;
    let (id, subject) = out.trim_end().split_once('\x1f')?;
    Some(format!("{id} “{subject}”"))
}

/// Entries in a sequencer todo list (comments and blank lines aside).
fn todo_count(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).count()
}

/// The merge, rebase, cherry-pick or revert in progress, if any.
pub fn operation(repo: &Path) -> GitResult<Option<Operation>> {
    let exists = |name: &str| git_path(repo, name).is_ok_and(|p| p.exists());
    let branch = run(repo, &["symbolic-ref", "-q", "--short", "HEAD"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let head_name = || branch.clone().unwrap_or_else(|| short_id(repo, "HEAD"));
    let merge_tool = run(repo, &["config", "merge.tool"]).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let before = || run(repo, &["rev-parse", "-q", "--verify", "ORIG_HEAD"]).ok().map(|s| short_id(repo, s.trim()));
    let op = if exists("rebase-merge") || exists("rebase-apply") {
        let dir = if exists("rebase-merge") { "rebase-merge" } else { "rebase-apply" };
        let file = |name: &str| read_git_file(repo, &format!("{dir}/{name}"));
        let rebased = file("head-name").and_then(|h| h.strip_prefix("refs/heads/").map(String::from));
        let onto = file("onto").map(|o| name_commit(repo, &o)).unwrap_or_else(|| "?".into());
        let (n, of) = if dir == "rebase-merge" { (file("msgnum"), file("end")) } else { (file("next"), file("last")) };
        let step = n.and_then(|n| n.parse().ok()).zip(of.and_then(|o| o.parse().ok()));
        // REBASE_HEAD is the commit that stopped; older gits only leave stopped-sha.
        let stopped = if exists("REBASE_HEAD") { Some("REBASE_HEAD".to_string()) } else { file("stopped-sha") };
        let commit = stopped.as_deref().and_then(|c| commit_title(repo, c));
        Operation {
            kind: OpType::Rebase,
            branch: rebased,
            ours: onto,
            theirs: commit.clone().unwrap_or_else(|| "the commit being replayed".into()),
            commit,
            step,
            remaining: 0,
            before: file("orig-head").map(|o| short_id(repo, &o)),
            merge_tool,
        }
    } else if exists("MERGE_HEAD") {
        Operation {
            kind: OpType::Merge,
            branch: branch.clone(),
            ours: head_name(),
            theirs: name_commit(repo, "MERGE_HEAD"),
            commit: None,
            step: None,
            remaining: 0,
            before: Some(short_id(repo, "HEAD")),
            merge_tool,
        }
    } else if exists("CHERRY_PICK_HEAD") || exists("REVERT_HEAD") {
        let pick = exists("CHERRY_PICK_HEAD");
        let head = if pick { "CHERRY_PICK_HEAD" } else { "REVERT_HEAD" };
        let commit = commit_title(repo, head);
        // The sequencer's todo list still includes the commit that stopped.
        let remaining = read_git_file(repo, "sequencer/todo").map(|t| todo_count(&t).saturating_sub(1)).unwrap_or(0);
        let theirs = if pick {
            commit.clone().unwrap_or_else(|| short_id(repo, head))
        } else {
            format!("before {}", short_id(repo, head))
        };
        Operation {
            kind: if pick { OpType::CherryPick } else { OpType::Revert },
            branch: branch.clone(),
            ours: head_name(),
            theirs,
            commit,
            step: None,
            remaining,
            before: before(),
            merge_tool,
        }
    } else {
        return Ok(None);
    };
    Ok(Some(op))
}

/// Carry on after the conflicts are resolved. A merge uses `message` when given
/// (else the prepared merge message); nothing ever opens an editor.
pub fn continue_operation(repo: &Path, kind: OpType, message: Option<&str>) -> GitResult<String> {
    let env = [("GIT_EDITOR", "true")];
    match (kind, message.map(str::trim).filter(|m| !m.is_empty())) {
        (OpType::Merge, Some(m)) => run_stdin_env(repo, &["commit", "-F", "-"], m, &env),
        _ => run_env(repo, &[kind.command(), "--continue"], &env),
    }
}

pub fn abort_operation(repo: &Path, kind: OpType) -> GitResult<String> {
    run(repo, &[kind.command(), "--abort"])
}

/// Leave out the commit that stopped and go on with the rest.
pub fn skip_operation(repo: &Path, kind: OpType) -> GitResult<String> {
    run_env(repo, &[kind.command(), "--skip"], &[("GIT_EDITOR", "true")])
}

fn run_env(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> GitResult<String> {
    let out = base_cmd(repo).args(args).envs(env.iter().copied()).output().map_err(|e| format!("Could not start git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err })
    }
}

pub(crate) fn run_bytes(repo: &Path, args: &[&str]) -> GitResult<Vec<u8>> {
    let out = base_cmd(repo).args(args).output().map_err(|e| format!("Could not start git: {e}"))?;
    if out.status.success() { Ok(out.stdout) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Which side a whole-file choice takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

/// Replace the whole working file with one side's version (it stays unresolved until marked).
pub fn take_side(repo: &Path, path: &str, side: Side) -> GitResult<String> {
    let flag = if side == Side::Ours { "--ours" } else { "--theirs" };
    run(repo, &["checkout", flag, "--", path])
}

pub fn mark_resolved(repo: &Path, path: &str) -> GitResult<String> {
    run(repo, &["add", "--", path])
}

/// Resolve a delete/modify (or one-sided) conflict by keeping the file: our
/// version when we have one, else theirs, else the common ancestor's.
pub fn keep_file(repo: &Path, path: &str) -> GitResult<String> {
    let stages = index_stages(repo, path)?;
    let stage = [2u8, 3, 1].into_iter().find(|s| stages.iter().any(|e| e.stage == *s)).ok_or("This file has no version to keep")?;
    run(repo, &["checkout-index", "-f", &format!("--stage={stage}"), "--", path])?;
    mark_resolved(repo, path)
}

/// Resolve a conflict by deleting the file.
pub fn delete_file(repo: &Path, path: &str) -> GitResult<String> {
    run(repo, &["rm", "-q", "-f", "--", path])
}

/// Write the resolved contents, then (optionally) mark the file resolved.
pub fn save_resolution(repo: &Path, path: &str, text: &str, mark: bool) -> GitResult<String> {
    std::fs::write(repo.join(path), text).map_err(|e| format!("Couldn't save {path}: {e}"))?;
    if mark { mark_resolved(repo, path) } else { Ok(String::new()) }
}

/// A choice for one conflicted file, applied (and marked resolved) by [`resolve_files`].
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Write these contents.
    Text(String, String),
    /// Take one side's whole version.
    Side(String, Side),
    /// Keep the file (delete/modify conflicts).
    Keep(String),
    Delete(String),
}

/// Apply each file's choice and mark it resolved. Carries on past a file that
/// fails, then reports every failure.
pub fn resolve_files(repo: &Path, choices: &[Resolution]) -> GitResult<String> {
    let mut errors = Vec::new();
    for c in choices {
        let res = match c {
            Resolution::Text(path, text) => save_resolution(repo, path, text, true),
            Resolution::Side(path, side) => take_side(repo, path, *side).and_then(|_| mark_resolved(repo, path)),
            Resolution::Keep(path) => keep_file(repo, path),
            Resolution::Delete(path) => delete_file(repo, path),
        };
        if let Err(e) = res {
            errors.push(e);
        }
    }
    if errors.is_empty() { Ok(String::new()) } else { Err(errors.join("\n")) }
}

/// Open the configured merge tool on one file and wait for it to close.
pub fn run_merge_tool(repo: &Path, path: &str) -> GitResult<String> {
    run(repo, &["mergetool", "--no-prompt", "--", path])
}

/// One version of a conflicted file in the index (stage 1 base, 2 ours, 3 theirs).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexStage {
    pub stage: u8,
    pub oid: String,
}

fn index_stages(repo: &Path, path: &str) -> GitResult<Vec<IndexStage>> {
    let out = run(repo, &["ls-files", "-u", "-z", "--", path])?;
    Ok(out
        .split('\0')
        .filter_map(|e| {
            // "<mode> <oid> <stage>\t<path>"
            let (meta, _) = e.split_once('\t')?;
            let f: Vec<&str> = meta.split(' ').collect();
            Some(IndexStage { oid: f.get(1)?.to_string(), stage: f.get(2)?.parse().ok()? })
        })
        .collect())
}

/// Text between conflict markers, line by line. Each line keeps its own ending
/// ("\n", "\r\n" or none at the very end), so writing it back changes nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub ours: Vec<String>,
    /// The common ancestor's lines (diff3 / zdiff3 markers, or read from the index).
    pub base: Option<Vec<String>>,
    pub theirs: Vec<String>,
    /// The marker lines exactly as found, to put an unresolved region back as it was.
    pub markers: [String; 4],
}

impl Region {
    /// The names git wrote after the opening and closing markers ("HEAD", "Stashed changes"…).
    pub fn labels(&self) -> (String, String) {
        let name = |m: &str| m.trim_end().trim_start_matches(['<', '>']).trim().to_string();
        (name(&self.markers[0]), name(&self.markers[3]))
    }

    /// The region as it is in the file, markers and all.
    pub fn original(&self) -> String {
        let mut s = self.markers[0].clone();
        s.extend(self.ours.iter().cloned());
        if let Some(base) = &self.base
            && !self.markers[1].is_empty()
        {
            s += &self.markers[1];
            s.extend(base.iter().cloned());
        }
        s += &self.markers[2];
        s.extend(self.theirs.iter().cloned());
        s += &self.markers[3];
        s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// Lines both sides agree on.
    Same(Vec<String>),
    Conflict(Region),
}

/// Split a conflicted file into agreed text and conflict regions. Understands the
/// default, diff3 and zdiff3 marker styles and any `conflict-marker-size`. A
/// region that never ends is left as ordinary text.
pub fn parse_conflicts(text: &str, marker_size: usize) -> Vec<Segment> {
    let is_marker = |line: &str, c: char| {
        let body = line.trim_end_matches(['\n', '\r']);
        body.len() >= marker_size
            && body.chars().take(marker_size).all(|x| x == c)
            && body[marker_size..].chars().next().is_none_or(|x| x == ' ')
    };
    #[derive(PartialEq)]
    enum In {
        Text,
        Ours,
        Base,
        Theirs,
    }
    let mut out = Vec::new();
    let mut same: Vec<String> = Vec::new();
    // The lines of the region being read (kept to fall back to plain text).
    let mut pending: Vec<String> = Vec::new();
    let mut region = Region { ours: vec![], base: None, theirs: vec![], markers: Default::default() };
    let mut state = In::Text;
    for line in text.split_inclusive('\n') {
        let l = line.to_string();
        match state {
            In::Text => {
                if is_marker(line, '<') {
                    region = Region { ours: vec![], base: None, theirs: vec![], markers: [l.clone(), String::new(), String::new(), String::new()] };
                    pending = vec![l];
                    state = In::Ours;
                } else {
                    same.push(l);
                }
                continue;
            }
            In::Ours | In::Base if is_marker(line, '=') => {
                region.markers[2] = l.clone();
                state = In::Theirs;
            }
            In::Ours if is_marker(line, '|') => {
                region.markers[1] = l.clone();
                region.base = Some(Vec::new());
                state = In::Base;
            }
            In::Ours => region.ours.push(l.clone()),
            In::Base => region.base.get_or_insert_with(Vec::new).push(l.clone()),
            In::Theirs if is_marker(line, '>') => {
                region.markers[3] = l;
                if !same.is_empty() {
                    out.push(Segment::Same(std::mem::take(&mut same)));
                }
                out.push(Segment::Conflict(std::mem::replace(&mut region, Region { ours: vec![], base: None, theirs: vec![], markers: Default::default() })));
                pending.clear();
                state = In::Text;
                continue;
            }
            In::Theirs => region.theirs.push(l.clone()),
        }
        pending.push(l);
    }
    // An unfinished region is just text.
    same.extend(pending);
    if !same.is_empty() {
        out.push(Segment::Same(same));
    }
    out
}

/// The file uses Windows line endings.
pub fn uses_crlf(text: &str) -> bool {
    text.contains("\r\n")
}

/// Everything the resolver needs about one conflicted file.
#[derive(Clone, Debug, PartialEq)]
pub struct ConflictFile {
    pub path: String,
    pub kind: ConflictKind,
    /// Binary (or not valid UTF-8): only whole-file choices.
    pub binary: bool,
    /// The working file as read (empty when binary or missing).
    pub text: String,
    pub segments: Vec<Segment>,
    /// The base was read from the index for each region (markers without a base section).
    pub index_base: bool,
    /// Size in bytes of the base, ours and theirs versions, where they exist.
    pub sizes: [Option<u64>; 3],
    /// Delete/modify and one-sided text files: what each side did, to compare.
    pub sided: Option<SidedView>,
}

/// A text file only one side still has, compared with the common ancestor.
#[derive(Clone, Debug, PartialEq)]
pub struct SidedView {
    /// The stage that still has the file: 2 (ours) or 3 (theirs); 1 when both deleted it.
    pub stage: u8,
    /// That version against the common ancestor, every line (all added when there's no ancestor).
    pub changes: Vec<DiffLine>,
    /// The common ancestor's lines: what the other side deleted.
    pub base: Vec<String>,
}

impl ConflictFile {
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Conflict(r) => Some(r),
            Segment::Same(_) => None,
        })
    }
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn marker_size(repo: &Path, path: &str) -> usize {
    // "path: conflict-marker-size: 9", or "unspecified".
    run(repo, &["check-attr", "conflict-marker-size", "--", path])
        .ok()
        .and_then(|o| o.trim_end().rsplit(": ").next().and_then(|v| v.parse().ok()))
        .unwrap_or(7)
}

/// Read a conflicted file: its kind, its regions and (when the markers don't
/// include it) each region's common-ancestor text, rebuilt from the index stages.
pub fn conflict_file(repo: &Path, path: &str, kind: ConflictKind) -> GitResult<ConflictFile> {
    let stages = index_stages(repo, path)?;
    let mut sizes = [None; 3];
    for s in &stages {
        if (1..=3).contains(&s.stage) {
            sizes[s.stage as usize - 1] = run(repo, &["cat-file", "-s", &s.oid]).ok().and_then(|n| n.trim().parse().ok());
        }
    }
    let bytes = std::fs::read(repo.join(path)).unwrap_or_default();
    let mut binary = looks_binary(&bytes);
    if !binary && bytes.is_empty() {
        // A missing working file: judge by whichever version the index has.
        if let Some(s) = stages.first() {
            binary = run_bytes(repo, &["cat-file", "blob", &s.oid]).is_ok_and(|b| looks_binary(&b));
        }
    }
    let mut file = ConflictFile { path: path.to_string(), kind, binary, text: String::new(), segments: vec![], index_base: false, sizes, sided: None };
    if binary {
        return Ok(file);
    }
    if !kind.has_regions() {
        file.sided = sided_view(repo, &stages).ok();
        return Ok(file);
    }
    file.text = String::from_utf8(bytes).unwrap_or_default();
    let size = marker_size(repo, path);
    file.segments = parse_conflicts(&file.text, size);
    let needs_base = file.regions().any(|r| r.base.is_none());
    // Only trusted when the rebuilt merge has as many regions as the file (it wasn't hand-edited).
    if needs_base
        && kind.has_regions()
        && let Ok(bases) = bases_from_index(repo, &stages, size)
        && bases.len() == file.regions().count()
    {
        let mut it = bases.into_iter();
        for seg in &mut file.segments {
            if let Segment::Conflict(r) = seg {
                let b = it.next();
                if r.base.is_none() {
                    r.base = b;
                }
            }
        }
        file.index_base = true;
    }
    Ok(file)
}

/// What happened to a file one side deleted: the surviving version against the ancestor.
fn sided_view(repo: &Path, stages: &[IndexStage]) -> GitResult<SidedView> {
    let oid = |n: u8| stages.iter().find(|s| s.stage == n).map(|s| s.oid.as_str());
    let stage = [2u8, 3].into_iter().find(|n| oid(*n).is_some()).unwrap_or(1);
    let text = |o: &str| run(repo, &["cat-file", "blob", o]);
    let base: Vec<String> = match oid(1) {
        Some(b) => text(b)?.split_inclusive('\n').map(|l| l.trim_end_matches(['\n', '\r']).to_string()).collect(),
        None => vec![],
    };
    let changes = match (oid(1), oid(stage)) {
        (Some(b), Some(k)) if stage != 1 => {
            // Whole-file context, so every line shows.
            let out = run(repo, &["diff", "--no-ext-diff", "-U1000000", b, k])?;
            let lines: Vec<DiffLine> = parse_patch(&out).into_iter().flat_map(|f| f.hunks).flat_map(|h| h.lines).collect();
            if lines.is_empty() {
                // Same as the ancestor.
                base.iter().enumerate().map(|(i, t)| DiffLine { kind: LineKind::Context, old_no: Some(i as u32 + 1), new_no: Some(i as u32 + 1), text: t.clone() }).collect()
            } else {
                lines
            }
        }
        (None, Some(k)) => text(k)?
            .split_inclusive('\n')
            .enumerate()
            .map(|(i, l)| DiffLine { kind: LineKind::Add, old_no: None, new_no: Some(i as u32 + 1), text: l.trim_end_matches(['\n', '\r']).to_string() })
            .collect(),
        _ => vec![],
    };
    Ok(SidedView { stage, changes, base })
}

/// Merge the three index versions again with diff3 markers to learn each region's base.
fn bases_from_index(repo: &Path, stages: &[IndexStage], marker_size: usize) -> GitResult<Vec<Vec<String>>> {
    let blob = |n: u8| -> GitResult<Vec<u8>> {
        match stages.iter().find(|s| s.stage == n) {
            Some(s) => run_bytes(repo, &["cat-file", "blob", &s.oid]),
            None => Ok(Vec::new()),
        }
    };
    let dir = std::env::temp_dir().join(format!("headway-merge-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut files = Vec::new();
        for (n, name) in [(2u8, "ours"), (1, "base"), (3, "theirs")] {
            let p = dir.join(name);
            std::fs::write(&p, blob(n)?).map_err(|e| e.to_string())?;
            files.push(p.to_string_lossy().into_owned());
        }
        let size = format!("--marker-size={marker_size}");
        // Exits with the number of conflicts, so any exit code is fine.
        let merged = run_any(repo, &["merge-file", "-p", "--diff3", &size, &files[0], &files[1], &files[2]])?;
        Ok(parse_conflicts(&merged, marker_size)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Conflict(r) => Some(r.base.unwrap_or_default()),
                Segment::Same(_) => None,
            })
            .collect())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

// ---------------------------------------------------------------------------
// Undo: what an action changed, so it can be put back

/// How an action's undo puts things back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UndoMode {
    /// Only refs moved (commit, amend, reword, branch delete, stash drop): move them back.
    /// The staging area is left alone, so an undone commit's changes are staged again.
    Refs,
    /// Refs and the staging area changed (reset --soft/--mixed): move the refs back and
    /// restore the staging area. Files on disk are left alone.
    Index,
    /// Refs and files changed (reset --hard, checkout, merge, rebase, cherry-pick,
    /// revert, pull): check the old files out the way `git checkout` would, then move
    /// the refs back.
    Tree,
    /// Only files on disk changed (discards): write the saved copies back.
    Files,
}

/// What to save before running an undoable action.
#[derive(Clone, Debug, PartialEq)]
pub enum Undoable {
    Refs,
    Index,
    /// `save_work`: the action throws away uncommitted changes (reset --hard), so keep them.
    Tree { save_work: bool },
    /// These files (repository-relative) will change on disk.
    Files(Vec<String>),
}

/// Where HEAD is: on a branch (full ref name) and/or at a commit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadState {
    pub branch: Option<String>,
    pub oid: Option<String>,
}

/// Every local branch and tag, HEAD and the stashes, at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RefState {
    pub head: HeadState,
    pub refs: std::collections::BTreeMap<String, String>,
    /// (commit, message), newest first.
    pub stashes: Vec<(String, String)>,
}

pub fn ref_state(repo: &Path) -> GitResult<RefState> {
    let refs = run(repo, &["for-each-ref", "--format=%(refname)%1f%(objectname)", "refs/heads", "refs/tags"])?
        .lines()
        .filter_map(|l| l.split_once('\x1f').map(|(n, o)| (n.to_string(), o.to_string())))
        .collect();
    let line = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    let head = HeadState {
        branch: run(repo, &["symbolic-ref", "-q", "HEAD"]).ok().and_then(line),
        oid: run(repo, &["rev-parse", "-q", "--verify", "HEAD"]).ok().and_then(line),
    };
    let stashes = run(repo, &["stash", "list", "--format=%H%x1f%gs"])?
        .lines()
        .filter_map(|l| l.split_once('\x1f').map(|(h, m)| (h.to_string(), m.to_string())))
        .collect();
    Ok(RefState { head, refs, stashes })
}

/// A branch or tag an action moved, created (no `before`) or deleted (no `after`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefMove {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// A remote branch already had `after` when the action ran.
    #[serde(default)]
    pub after_on_remote: bool,
}

/// A file a discard changed, as saved blobs (`None`: the file didn't exist).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileCopy {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    #[serde(default)]
    pub executable: bool,
}

/// One undoable action in the operation log. Object ids are full SHAs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: u64,
    /// What was done, to follow "Undo: " ("discard hunk in src/app.rs").
    pub what: String,
    pub repo: PathBuf,
    /// Unix seconds.
    pub time: i64,
    pub mode: UndoMode,
    pub head_before: HeadState,
    pub head_after: HeadState,
    #[serde(default)]
    pub refs: Vec<RefMove>,
    /// Stashes the action removed: (commit, message).
    #[serde(default)]
    pub dropped_stashes: Vec<(String, String)>,
    /// The staging area before the action, as a tree.
    #[serde(default)]
    pub index: Option<String>,
    /// Uncommitted work thrown away by the action, as a stash commit.
    #[serde(default)]
    pub saved_work: Option<String>,
    #[serde(default)]
    pub files: Vec<FileCopy>,
    /// Why this can't be undone (something couldn't be saved beforehand).
    #[serde(default)]
    pub not_undoable: Option<String>,
    #[serde(default)]
    pub undone: bool,
}

/// What was saved before an action ran; [`finish_undo`] turns it into an entry.
pub struct UndoPrep {
    mode: UndoMode,
    before: RefState,
    index: Option<String>,
    saved_work: Option<String>,
    /// (path, saved blob, executable)
    files: Vec<(String, Option<String>, bool)>,
    problem: Option<String>,
}

/// Save what an action is about to change. Runs before the action.
pub fn prepare_undo(repo: &Path, what: &Undoable) -> GitResult<UndoPrep> {
    let mode = match what {
        Undoable::Refs => UndoMode::Refs,
        Undoable::Index => UndoMode::Index,
        Undoable::Tree { .. } => UndoMode::Tree,
        Undoable::Files(_) => UndoMode::Files,
    };
    let mut prep = UndoPrep { mode, before: ref_state(repo)?, index: None, saved_work: None, files: Vec::new(), problem: None };
    match what {
        Undoable::Refs => {}
        Undoable::Index => match run(repo, &["write-tree"]) {
            Ok(t) => prep.index = Some(t.trim().to_string()),
            Err(_) => prep.problem = Some("the staging area had unresolved conflicts, which can't be saved".into()),
        },
        Undoable::Tree { save_work } => {
            if *save_work {
                // A commit holding the staged and unstaged changes, without touching them.
                match run(repo, &["stash", "create"]) {
                    Ok(s) => prep.saved_work = Some(s.trim().to_string()).filter(|s| !s.is_empty()),
                    Err(_) => prep.problem = Some("your uncommitted changes couldn't be saved first".into()),
                }
            }
        }
        Undoable::Files(paths) => {
            for p in paths {
                match save_file(repo, p, true) {
                    Ok(Some((blob, exec))) => prep.files.push((p.clone(), blob, exec)),
                    Ok(None) => {}
                    Err(e) => prep.problem = Some(e),
                }
            }
        }
    }
    Ok(prep)
}

/// A file's contents as a blob id, byte for byte (no line-ending or filter changes), and
/// whether it's executable. `write` keeps the blob in the object database.
/// `Ok(None)` for a folder (a submodule), which has no contents to save.
#[allow(clippy::type_complexity)]
fn save_file(repo: &Path, path: &str, write: bool) -> Result<Option<(Option<String>, bool)>, String> {
    let full = repo.join(path);
    let Ok(meta) = std::fs::symlink_metadata(&full) else { return Ok(Some((None, false))) };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.file_type().is_symlink() {
        return Err(format!("{path} is a symbolic link, which can't be saved"));
    }
    let mut args = vec!["hash-object", "--no-filters"];
    if write {
        args.push("-w");
    }
    args.extend(["--", path]);
    let blob = run(repo, &args).map_err(|e| format!("{path} couldn't be saved: {e}"))?;
    Ok(Some((Some(blob.trim().to_string()), is_executable(&meta))))
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Compare with what [`prepare_undo`] saved, after the action ran. `None` when the
/// action changed nothing.
pub fn finish_undo(repo: &Path, prep: UndoPrep, what: &str) -> GitResult<Option<UndoEntry>> {
    let after = ref_state(repo)?;
    let before = &prep.before;
    let mut names: Vec<&String> = before.refs.keys().chain(after.refs.keys()).collect();
    names.sort();
    names.dedup();
    let mut refs = Vec::new();
    for n in names {
        let (b, a) = (before.refs.get(n), after.refs.get(n));
        if b != a {
            let after_on_remote = match a {
                Some(a) => !run(repo, &["for-each-ref", "--contains", a, "--format=%(refname)", "refs/remotes"]).unwrap_or_default().trim().is_empty(),
                None => false,
            };
            refs.push(RefMove { name: n.clone(), before: b.cloned(), after: a.cloned(), after_on_remote });
        }
    }
    let dropped_stashes: Vec<_> = before.stashes.iter().filter(|s| !after.stashes.iter().any(|a| a.0 == s.0)).cloned().collect();
    let mut files = Vec::new();
    for (path, blob, executable) in prep.files {
        let now = save_file(repo, &path, false)?.and_then(|(b, _)| b);
        if now != blob {
            files.push(FileCopy { path, before: blob, after: now, executable });
        }
    }
    // A reset to where HEAD already is only changes the staging area or the files.
    let index_changed = prep.mode == UndoMode::Index && prep.index.as_deref() != run(repo, &["write-tree"]).ok().as_deref().map(str::trim);
    let changed = !refs.is_empty()
        || !dropped_stashes.is_empty()
        || !files.is_empty()
        || before.head != after.head
        || index_changed
        || prep.saved_work.is_some();
    if !changed {
        return Ok(None);
    }
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    Ok(Some(UndoEntry {
        id: 0,
        what: what.to_string(),
        repo: repo.to_path_buf(),
        time,
        mode: prep.mode,
        head_before: before.head.clone(),
        head_after: after.head,
        refs,
        dropped_stashes,
        index: prep.index,
        saved_work: prep.saved_work,
        files,
        not_undoable: prep.problem,
        undone: false,
    }))
}

fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/heads/").or_else(|| name.strip_prefix("refs/tags/")).unwrap_or(name)
}

fn short_oid(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

fn object_exists(repo: &Path, oid: &str) -> bool {
    run(repo, &["cat-file", "-e", oid]).is_ok()
}

/// Put back what an entry's action changed. Checks everything first and changes
/// nothing when it can't be done safely; the error says why.
pub fn undo(repo: &Path, e: &UndoEntry) -> GitResult<String> {
    if let Some(why) = &e.not_undoable {
        return Err(format!("This can't be undone: {why}."));
    }
    if e.mode == UndoMode::Files {
        restore_files(repo, &e.files)?;
        return Ok(format!("Undid {}", e.what));
    }
    if operation(repo)?.is_some() {
        return Err("A merge, rebase, cherry-pick or revert is in progress. Finish or abort it first.".into());
    }

    // Everything must still be where the action left it.
    let now = ref_state(repo)?;
    for m in &e.refs {
        let cur = now.refs.get(&m.name);
        if cur != m.after.as_ref() {
            let name = short_ref(&m.name);
            return Err(match (cur, &m.after) {
                (Some(c), Some(_)) => format!("{name} has moved since (it's at {} now), so this can't be undone safely.", short_oid(c)),
                (None, _) => format!("{name} has been deleted since, so this can't be undone safely."),
                (Some(_), None) => format!("A new {name} has been created since, so this can't be undone safely."),
            });
        }
    }
    // Moving refs back only needs HEAD on the same branch; putting back the staging area
    // or files needs it at the same commit too.
    let same_commit_needed = e.mode != UndoMode::Refs || e.head_after.branch.is_none();
    if now.head.branch != e.head_after.branch || (same_commit_needed && now.head.oid != e.head_after.oid) {
        let at = match (&now.head.branch, &now.head.oid) {
            (Some(b), _) if now.head.branch != e.head_after.branch => format!("you're on {} now", short_ref(b)),
            (_, Some(o)) => format!("it's at {} now", short_oid(o)),
            _ => "it has changed".into(),
        };
        return Err(format!("HEAD has moved since ({at}), so this can't be undone safely."));
    }
    // Taking back commits that have been pushed since would rewrite shared history.
    for m in e.refs.iter().filter(|m| m.name.starts_with("refs/heads/") && !m.after_on_remote) {
        let Some(after) = &m.after else { continue };
        let kept = m.before.as_ref().is_some_and(|b| run(repo, &["merge-base", "--is-ancestor", after, b]).is_ok());
        if kept {
            continue;
        }
        let remotes = run(repo, &["for-each-ref", "--contains", after, "--format=%(refname:short)", "refs/remotes"])?;
        if let Some(r) = remotes.lines().find(|l| !l.trim().is_empty()) {
            return Err(format!(
                "{} has been pushed to {r} since. Undoing would take back commits that are already shared, so it's left as it is.",
                short_ref(&m.name)
            ));
        }
    }
    let mut needed: Vec<&String> = e.refs.iter().filter_map(|m| m.before.as_ref()).collect();
    needed.extend(e.head_before.oid.as_ref());
    needed.extend(e.index.as_ref());
    needed.extend(e.saved_work.as_ref());
    needed.extend(e.dropped_stashes.iter().map(|s| &s.0));
    if let Some(gone) = needed.iter().find(|o| !object_exists(repo, o)) {
        return Err(format!("Git has cleaned up {} since, so this can no longer be undone.", short_oid(gone)));
    }

    // Files first: if they can't be put back, nothing has changed yet.
    if e.mode == UndoMode::Tree {
        if let (Some(cur), Some(old)) = (&now.head.oid, &e.head_before.oid) {
            if cur != old {
                run(repo, &["read-tree", "-m", "-u", cur, old]).map_err(|err| {
                    format!("Your uncommitted changes would be overwritten. Commit or stash them, then undo again.\n\n{err}")
                })?;
            }
        }
    }
    let msg = format!("Headway: undo {}", e.what);
    let mut tx = String::new();
    for m in &e.refs {
        match (&m.before, &m.after) {
            (Some(b), Some(a)) => tx.push_str(&format!("update {} {b} {a}\n", m.name)),
            (Some(b), None) => tx.push_str(&format!("create {} {b}\n", m.name)),
            (None, Some(a)) => tx.push_str(&format!("delete {} {a}\n", m.name)),
            (None, None) => {}
        }
    }
    let detached = |h: &HeadState| h.branch.is_none();
    if detached(&e.head_after) && detached(&e.head_before) && e.head_before.oid != e.head_after.oid {
        if let (Some(b), Some(a)) = (&e.head_before.oid, &e.head_after.oid) {
            tx.push_str(&format!("update HEAD {b} {a}\n"));
        }
    }
    if !tx.is_empty() {
        run_stdin(repo, &["update-ref", "--no-deref", "-m", &msg, "--stdin"], &tx)?;
    }
    if e.head_before.branch != e.head_after.branch {
        match (&e.head_before.branch, &e.head_before.oid) {
            (Some(b), _) => run(repo, &["symbolic-ref", "-m", &msg, "HEAD", b]).map(drop)?,
            (None, Some(o)) => run(repo, &["update-ref", "--no-deref", "-m", &msg, "HEAD", o]).map(drop)?,
            (None, None) => {}
        }
    }
    if e.mode == UndoMode::Index {
        if let Some(t) = &e.index {
            run(repo, &["read-tree", t])?;
        }
    }
    if let Some(w) = &e.saved_work {
        run(repo, &["stash", "apply", "--index", w]).map_err(|err| format!("The branch is back, but your saved changes couldn't be reapplied ({}): {err}", short_oid(w)))?;
    }
    // Oldest first, so the newest dropped stash ends up on top.
    for (oid, message) in e.dropped_stashes.iter().rev() {
        run(repo, &["stash", "store", "-m", message, oid])?;
    }
    Ok(format!("Undid {}", e.what))
}

/// Write discarded files back, if each one is still exactly as the discard left it.
fn restore_files(repo: &Path, files: &[FileCopy]) -> GitResult<()> {
    for f in files {
        let now = save_file(repo, &f.path, false)?.and_then(|(b, _)| b);
        if now != f.after {
            return Err(format!("{} has changed since, so it's left as it is.", f.path));
        }
        if let Some(b) = &f.before {
            if !object_exists(repo, b) {
                return Err(format!("Git has cleaned up the saved copy of {} since, so this can no longer be undone.", f.path));
            }
        }
    }
    for f in files {
        let full = repo.join(&f.path);
        match &f.before {
            Some(b) => {
                let bytes = run_bytes(repo, &["cat-file", "blob", b])?;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&full, bytes).map_err(|e| format!("Couldn't write {}: {e}", f.path))?;
                set_executable(&full, f.executable);
            }
            None => std::fs::remove_file(&full).map_err(|e| format!("Couldn't remove {}: {e}", f.path))?,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut p = meta.permissions();
        let mode = p.mode();
        p.set_mode(if exec { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 });
        let _ = std::fs::set_permissions(path, p);
    }
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _exec: bool) {}

// ---------------------------------------------------------------------------
// Reflog

#[derive(Clone, Debug, PartialEq)]
pub struct ReflogEntry {
    pub hash: String,
    /// "commit", "checkout", "reset", "rebase (finish)"…
    pub action: String,
    pub message: String,
    /// Unix seconds when the ref moved.
    pub time: i64,
}

/// Where `refname` (HEAD or a full branch name) has pointed, newest first.
pub fn reflog(repo: &Path, refname: &str, limit: usize) -> GitResult<Vec<ReflogEntry>> {
    let n = format!("-n{limit}");
    let out = run(repo, &["reflog", "show", &n, "--date=unix", "--format=%H%x1f%gd%x1f%gs", refname, "--"])?;
    Ok(out.lines().filter_map(parse_reflog_line).collect())
}

fn parse_reflog_line(line: &str) -> Option<ReflogEntry> {
    let mut f = line.splitn(3, '\x1f');
    let hash = f.next()?.to_string();
    let selector = f.next()?;
    let subject = f.next().unwrap_or("");
    let time = selector.rsplit_once('{').and_then(|(_, t)| t.trim_end_matches('}').parse().ok()).unwrap_or(0);
    let (action, message) = match subject.split_once(": ") {
        Some((a, m)) => (a.to_string(), m.to_string()),
        None => (subject.to_string(), String::new()),
    };
    Some(ReflogEntry { hash, action, message, time })
}

// ---------------------------------------------------------------------------
// Blame and file history

/// Run git and hand each line of its output to `on_line` as soon as it arrives
/// (for slow commands worth showing early, like blame).
pub fn stream(repo: &Path, args: &[&str], mut on_line: impl FnMut(&str)) -> GitResult<()> {
    let mut child = base_cmd(repo).args(args).spawn().map_err(|e| format!("Could not start git: {e}"))?;
    // stderr is read on its own thread, so a chatty git can't fill the pipe and stall.
    let mut err_pipe = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err_pipe.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    if let Some(out) = child.stdout.take() {
        let mut r = BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => on_line(String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r'])),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.to_string());
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let err = err.trim();
        Err(if err.is_empty() { format!("git {} failed", args.join(" ")) } else { err.to_string() })
    }
}

/// A commit that blame names as the last to change some lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlameCommit {
    pub hash: String,
    pub author: String,
    pub email: String,
    /// Unix seconds (author date).
    pub time: i64,
    pub summary: String,
    /// The commit before it and the file's name there ("Blame the parent").
    pub previous: Option<(String, String)>,
    /// The oldest commit blame looked at: its lines may be older still.
    pub boundary: bool,
}

impl BlameCommit {
    /// Lines changed in the working copy but not committed (git's all-zero id).
    pub fn uncommitted(&self) -> bool {
        !self.hash.is_empty() && self.hash.bytes().all(|b| b == b'0')
    }

    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(8)]
    }
}

/// Consecutive lines that one commit last changed.
#[derive(Clone, Debug, PartialEq)]
pub struct BlameRange {
    /// Index into [`BlameParser::commits`].
    pub commit: usize,
    /// First line (from 1) in that commit's version of the file.
    pub orig_line: u32,
    /// First line (from 1) in the blamed version of the file.
    pub line: u32,
    pub count: u32,
    /// The file's name in that commit (it may have been renamed since).
    pub path: String,
}

/// Reads `git blame --incremental` output a line at a time, so results can be
/// shown while git is still working.
#[derive(Default)]
pub struct BlameParser {
    pub commits: Vec<BlameCommit>,
    index: HashMap<String, usize>,
    cur: Option<BlameRange>,
}

impl BlameParser {
    /// Read one line. Returns a range once its entry is complete.
    pub fn feed(&mut self, line: &str) -> Option<BlameRange> {
        let Some(cur) = &mut self.cur else {
            // "<sha> <orig line> <final line> <count>" starts an entry.
            let mut it = line.split(' ');
            let hash = it.next()?;
            if hash.len() < 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let orig_line = it.next()?.parse().ok()?;
            let line_no = it.next()?.parse().ok()?;
            let count = it.next().and_then(|c| c.parse().ok()).unwrap_or(1);
            let commit = match self.index.get(hash) {
                Some(&i) => i,
                None => {
                    self.commits.push(BlameCommit { hash: hash.to_string(), ..Default::default() });
                    self.index.insert(hash.to_string(), self.commits.len() - 1);
                    self.commits.len() - 1
                }
            };
            self.cur = Some(BlameRange { commit, orig_line, line: line_no, count, path: String::new() });
            return None;
        };
        let c = &mut self.commits[cur.commit];
        let (key, val) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "author" => c.author = val.to_string(),
            "author-mail" => c.email = val.trim_start_matches('<').trim_end_matches('>').to_string(),
            "author-time" => c.time = val.parse().unwrap_or(0),
            "summary" => c.summary = val.to_string(),
            "boundary" => c.boundary = true,
            "previous" => {
                if let Some((h, p)) = val.split_once(' ') {
                    c.previous = Some((h.to_string(), unquote_path(p)));
                }
            }
            // "filename" always ends an entry.
            "filename" => {
                cur.path = unquote_path(val);
                return self.cur.take();
            }
            _ => {}
        }
        None
    }
}

/// How far blame looks for lines that moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum BlameMoves {
    #[default]
    Off,
    /// Lines moved within the file (`-M`).
    File,
    /// Lines moved or copied from other files changed in the same commit (`-C`).
    Files,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlameOptions {
    /// `-w`: whitespace-only changes don't count.
    pub ignore_whitespace: bool,
    pub moves: BlameMoves,
    /// Skip the commits listed in the repository's ignore-revs file.
    pub ignore_revs: bool,
}

impl Default for BlameOptions {
    fn default() -> Self {
        Self { ignore_whitespace: false, moves: BlameMoves::Off, ignore_revs: true }
    }
}

/// The list of commits blame should skip (formatting commits and the like).
#[derive(Clone, Debug, PartialEq)]
pub struct IgnoreRevs {
    /// As written in the setting, or the conventional file name.
    pub file: String,
    /// Set with `blame.ignoreRevsFile` (git then applies it by itself).
    pub configured: bool,
    pub exists: bool,
}

/// The repository's ignore-revs file: `blame.ignoreRevsFile` when set, otherwise a
/// `.git-blame-ignore-revs` in the top folder (the name GitHub and GitLab look for).
pub fn ignore_revs(repo: &Path) -> Option<IgnoreRevs> {
    let set = run(repo, &["config", "--get-all", "blame.ignoreRevsFile"]).unwrap_or_default();
    // An empty value clears the earlier ones.
    let configured = set.lines().rev().take_while(|l| !l.trim().is_empty()).last().map(|l| l.trim().to_string());
    let top = run(repo, &["rev-parse", "--show-toplevel"]).map(|t| PathBuf::from(t.trim())).unwrap_or_else(|_| repo.to_path_buf());
    if let Some(file) = configured {
        let p = match file.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => repo.join(&file),
        };
        return Some(IgnoreRevs { exists: p.is_file(), file, configured: true });
    }
    let conventional = ".git-blame-ignore-revs";
    top.join(conventional).is_file().then(|| IgnoreRevs { file: conventional.into(), configured: false, exists: true })
}

/// Arguments for `git blame --incremental` of `path` at `rev` (the working copy when `None`).
pub fn blame_args(rev: Option<&str>, path: &str, opts: &BlameOptions, ignore: Option<&IgnoreRevs>) -> Vec<String> {
    let mut a: Vec<String> = vec!["blame".into(), "--incremental".into()];
    if opts.ignore_whitespace {
        a.push("-w".into());
    }
    match opts.moves {
        BlameMoves::Off => {}
        BlameMoves::File => a.push("-M".into()),
        BlameMoves::Files => a.push("-C".into()),
    }
    match ignore {
        // The setting applies by itself; an empty name clears it.
        Some(i) if i.configured && !opts.ignore_revs => a.push("--ignore-revs-file=".into()),
        Some(i) if !i.configured && opts.ignore_revs => a.push(format!("--ignore-revs-file={}", i.file)),
        _ => {}
    }
    if let Some(r) = rev {
        a.push(r.to_string());
    }
    a.push("--".into());
    a.push(path.to_string());
    a
}

/// The text blame numbers its lines against: the file at `rev`, or the working copy.
/// `Err` for binary files.
/// The blob id of `path` at `rev`, or of the file in the working folder when `rev` is `None`.
pub fn blob_id(repo: &Path, rev: Option<&str>, path: &str) -> GitResult<String> {
    let out = match rev {
        Some(r) => run(repo, &["rev-parse", "--verify", "--quiet", &format!("{r}:{path}")])?,
        None => run(repo, &["hash-object", "--", path])?,
    };
    Ok(out.trim().to_string())
}

pub fn blame_text(repo: &Path, rev: Option<&str>, path: &str) -> GitResult<Vec<String>> {
    let bytes = match rev {
        Some(r) => {
            let out = base_cmd(repo)
                .args(["cat-file", "blob", &format!("{r}:{path}")])
                .output()
                .map_err(|e| format!("Could not start git: {e}"))?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            out.stdout
        }
        None => std::fs::read(repo.join(path)).map_err(|e| format!("Couldn't read {path}: {e}"))?,
    };
    if looks_binary(&bytes) {
        return Err("This is a binary file, so it has no lines to blame.".into());
    }
    Ok(String::from_utf8_lossy(&bytes).lines().map(String::from).collect())
}

/// Blame `path` at `rev`, calling `on_range` for each block of lines as git finds it
/// (not in file order). Returns every commit seen.
pub fn blame(
    repo: &Path,
    rev: Option<&str>,
    path: &str,
    opts: &BlameOptions,
    ignore: Option<&IgnoreRevs>,
    mut on_range: impl FnMut(&BlameParser, BlameRange),
) -> GitResult<Vec<BlameCommit>> {
    if let Some(i) = ignore.filter(|i| i.configured && !i.exists) {
        return Err(format!(
            "blame.ignoreRevsFile is set to {}, which doesn't exist, so git can't blame. Create the file or remove the setting.",
            i.file
        ));
    }
    let args = blame_args(rev, path, opts, ignore);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut parser = BlameParser::default();
    stream(repo, &args, |line| {
        if let Some(r) = parser.feed(line) {
            on_range(&parser, r);
        }
    })?;
    Ok(parser.commits)
}

/// A commit in a file's (or folder's, or lines') history.
#[derive(Clone, Debug, PartialEq)]
pub struct FileCommit {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub summary: String,
    /// The file's name in this commit.
    pub path: String,
    /// Its earlier name, when this commit renamed it.
    pub old_path: Option<String>,
    pub change: Change,
}

/// A commit in a history, with its change to the lines when it's line history.
pub type HistoryEntry = (FileCommit, Option<DiffFile>);

const FILE_LOG_FORMAT: &str = "--format=%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%s";

fn file_commit_header(head: &str, path: &str) -> Option<FileCommit> {
    let f: Vec<&str> = head.splitn(5, '\x1f').collect();
    if f.len() < 5 {
        return None;
    }
    Some(FileCommit {
        hash: f[0].to_string(),
        author: f[1].to_string(),
        email: f[2].to_string(),
        time: f[3].parse().unwrap_or(0),
        summary: f[4].trim_end().to_string(),
        path: path.to_string(),
        old_path: None,
        change: Change::Modified,
    })
}

/// Commits that changed `path`, newest first. A file is followed across renames
/// (each commit knows the name it had then); a folder isn't.
pub fn file_log(repo: &Path, path: &str, folder: bool, limit: usize) -> GitResult<Vec<FileCommit>> {
    let n = format!("-n{limit}");
    let mut args = vec!["log", FILE_LOG_FORMAT, "--name-status", "-M", &n];
    if !folder {
        args.push("--follow");
    }
    args.extend(["--", path]);
    let out = run(repo, &args)?;
    Ok(parse_file_log(&out, path, folder))
}

pub fn parse_file_log(out: &str, path: &str, folder: bool) -> Vec<FileCommit> {
    // Newest first: once a commit renames the file, older ones use the old name.
    let mut name = path.to_string();
    let mut list = Vec::new();
    for chunk in out.split('\x1e').filter(|c| !c.trim().is_empty()) {
        let mut lines = chunk.lines();
        let Some(mut c) = file_commit_header(lines.next().unwrap_or(""), &name) else { continue };
        if !folder {
            for l in lines.filter(|l| !l.is_empty()) {
                let f: Vec<&str> = l.split('\t').collect();
                let Some(change) = f.first().and_then(|s| s.chars().next()).and_then(Change::from_code) else { continue };
                match (&change, f.as_slice()) {
                    (Change::Renamed | Change::Copied, [_, old, new]) => {
                        c.path = unquote_path(new);
                        c.old_path = Some(unquote_path(old));
                        name = unquote_path(old);
                    }
                    (_, [_, p]) => {
                        c.path = unquote_path(p);
                        name = c.path.clone();
                    }
                    _ => {}
                }
                c.change = change;
            }
        }
        list.push(c);
    }
    list
}

/// What one commit changed in `paths` (a file under its old and new names, or a folder).
pub fn commit_files(repo: &Path, hash: &str, paths: &[String], ignore_whitespace: bool) -> GitResult<Vec<DiffFile>> {
    let mut args = vec!["show", "--no-ext-diff", "--full-index", "--format=", "--patch", "-M", "--diff-merges=first-parent"];
    args.extend(ws_args(ignore_whitespace));
    args.extend([hash, "--"]);
    args.extend(paths.iter().map(String::as_str));
    let mut files = parse_patch(&run(repo, &args)?);
    note_whitespace_only(&mut files, ignore_whitespace);
    if ignore_whitespace {
        files = keep_whitespace_only_files(repo, hash, paths, files)?;
    }
    Ok(files)
}

/// Commits that changed lines `start..=end` of `path` as it is at `rev`, newest
/// first, each with the change to just those lines (`git log -L`).
pub fn line_log(repo: &Path, rev: &str, path: &str, start: u32, end: u32, limit: usize) -> GitResult<Vec<HistoryEntry>> {
    let range = format!("-L{start},{end}:{path}");
    let n = format!("-n{limit}");
    let out = run(repo, &["log", FILE_LOG_FORMAT, &n, &range, rev])?;
    Ok(parse_line_log(&out, path))
}

pub fn parse_line_log(out: &str, path: &str) -> Vec<HistoryEntry> {
    let mut list = Vec::new();
    for chunk in out.split('\x1e').filter(|c| !c.trim().is_empty()) {
        let (head, patch) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let Some(mut c) = file_commit_header(head, path) else { continue };
        let file = parse_patch(patch).into_iter().next();
        if let Some(f) = &file {
            c.path = f.path.clone();
            c.change = f.change.clone();
            // -L leaves out rename headers: the two names tell.
            let old = f.header.lines().next().and_then(|l| l.strip_prefix("diff --git a/")).and_then(|r| r.rsplit_once(" b/")).map(|(a, _)| a.to_string());
            if old.as_deref().is_some_and(|o| o != f.path) && f.change != Change::Added {
                c.old_path = old;
                c.change = Change::Renamed;
            }
        }
        list.push((c, file));
    }
    list
}

/// Line history for lines of the working copy: git starts from the last commit,
/// so the line numbers are first moved past your uncommitted changes.
pub fn working_line_log(repo: &Path, path: &str, start: u32, end: u32, limit: usize) -> GitResult<Vec<HistoryEntry>> {
    let diff = run(repo, &["diff", "--no-ext-diff", "HEAD", "--", path])?;
    let (s, e) = match parse_patch(&diff).first() {
        Some(f) => (line_before(f, start), line_before(f, end)),
        None => (start, end),
    };
    line_log(repo, "HEAD", path, s, e.max(s), limit)
}

/// Where line `new` of a changed file was before the change (line history of the
/// working copy has to start from a commit). Added lines map to the nearest line
/// that existed before them.
pub fn line_before(file: &DiffFile, new: u32) -> u32 {
    // Lines after a hunk move by what it added or removed.
    let mut shift: i64 = 0;
    for h in &file.hunks {
        let (old_start, new_start) = parse_hunk_starts(&h.header);
        let old_count = h.lines.iter().filter(|l| l.old_no.is_some()).count() as u32;
        let new_count = h.lines.iter().filter(|l| l.new_no.is_some()).count() as u32;
        // A hunk that only removes lines sits just after `new_start`.
        if new < if new_count == 0 { new_start + 1 } else { new_start } {
            break;
        }
        if new < new_start + new_count {
            let mut last_old = if old_count == 0 { old_start } else { old_start.saturating_sub(1) };
            for l in &h.lines {
                if let Some(o) = l.old_no {
                    last_old = o;
                }
                if l.new_no == Some(new) {
                    return l.old_no.unwrap_or(last_old).max(1);
                }
            }
            return last_old.max(1);
        }
        let after = |start: u32, count: u32| if count == 0 { start + 1 } else { start + count } as i64;
        shift = after(old_start, old_count) - after(new_start, new_count);
    }
    (new as i64 + shift).max(1) as u32
}

/// Every tracked file, for the "Find file" list.
pub fn tracked_files(repo: &Path) -> GitResult<Vec<String>> {
    let out = run(repo, &["ls-files", "-z"])?;
    Ok(out.split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("headway-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]).unwrap();
        run(&dir, &["config", "user.name", "Test"]).unwrap();
        run(&dir, &["config", "user.email", "t@example.com"]).unwrap();
        run(&dir, &["config", "commit.gpgsign", "false"]).unwrap();
        dir
    }

    #[test]
    fn diffs_name_both_blobs_and_they_can_be_read_and_highlighted() {
        use crate::highlight::{self, Job, JobFile, Tok};
        let dir = temp_repo("blob-ids");
        let before = "fn a() {\n    let s = \"one\n two\";\n}\n";
        std::fs::write(dir.join("a.rs"), before).unwrap();
        std::fs::write(dir.join("big.bin"), vec![0u8; 5000]).unwrap();
        stage(&dir, &["a.rs".into(), "big.bin".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        let after = before.replace(" two", " three");
        std::fs::write(dir.join("a.rs"), &after).unwrap();

        let head_id = run(&dir, &["rev-parse", "HEAD:a.rs"]).unwrap().trim().to_string();
        let disk_id = run(&dir, &["hash-object", "a.rs"]).unwrap().trim().to_string();
        let d = file_diff(&dir, "a.rs", false, false).unwrap().unwrap();
        assert_eq!(d.old_id.as_deref(), Some(head_id.as_str()), "full ids, not abbreviated");
        assert_eq!(d.new_id.as_deref(), Some(disk_id.as_str()), "git names the working copy too");
        assert_eq!(blob_id(&dir, Some("HEAD"), "a.rs").unwrap(), head_id);
        assert_eq!(blob_id(&dir, None, "a.rs").unwrap(), disk_id);

        // A new file has no old side.
        std::fs::write(dir.join("n.rs"), "fn n() {}\n").unwrap();
        let n = file_diff(&dir, "n.rs", false, true).unwrap().unwrap();
        assert_eq!((n.old_id, n.new_id.is_some()), (None, true));

        // Sizes and contents, skipping what's too big and what git doesn't have.
        let big = run(&dir, &["rev-parse", "HEAD:big.bin"]).unwrap().trim().to_string();
        let missing = "1".repeat(40);
        let sizes = blob_sizes(&dir, &[head_id.clone(), big.clone(), missing.clone()]).unwrap();
        assert_eq!(sizes.get(&big), Some(&5000));
        assert!(!sizes.contains_key(&missing));
        let texts = small_blobs(&dir, &[head_id.clone(), big.clone()], 1000).unwrap();
        assert_eq!(texts.get(&head_id).map(String::as_str), Some(before));
        assert!(!texts.contains_key(&big));

        // Highlighting both sides: the old one from git, the new one from disk.
        let slots = std::sync::Arc::new(vec![std::sync::OnceLock::new()]);
        let file = JobFile { path: "a.rs".into(), old_path: "a.rs".into(), old_id: d.old_id.clone(), new_id: d.new_id.clone(), new_on_disk: true };
        Job { repo: dir.clone(), slots: slots.clone(), files: vec![(0, file)] }.run(&|| {});
        let sides = slots[0].get().unwrap();
        let line = |h: &highlight::Highlighted, n: u32, text: &str| h.line(n, text).unwrap().iter().map(|s| s.tok).collect::<Vec<_>>();
        assert_eq!(line(sides.old.as_ref().unwrap(), 3, " two\";"), vec![Tok::String], "the second line of a string is coloured as string");
        assert_eq!(line(sides.new.as_ref().unwrap(), 3, " three\";"), vec![Tok::String]);
        assert!(highlight::is_cached(&head_id, "a.rs") && highlight::is_cached(&disk_id, "a.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn highlighting_stops_when_nobody_wants_the_diff() {
        use crate::highlight::{self, Job, JobFile};
        let dir = temp_repo("hl-cancel");
        // Contents no other test has, so its blob id is only ever seen here.
        let text = format!("fn cancelled_{}() {{}}\n", std::process::id());
        std::fs::write(dir.join("c.rs"), &text).unwrap();
        stage(&dir, &["c.rs".into()]).unwrap();
        let id = run(&dir, &["rev-parse", ":c.rs"]).unwrap().trim().to_string();
        let file = || JobFile { path: "c.rs".into(), old_path: "c.rs".into(), old_id: Some(id.clone()), new_id: None, new_on_disk: false };

        // The diff was closed: the job holds the only reference to its slots.
        let slots = std::sync::Arc::new(vec![std::sync::OnceLock::new()]);
        let weak = std::sync::Arc::downgrade(&slots);
        Job { repo: dir.clone(), slots, files: vec![(0, file())] }.run(&|| {});
        assert!(weak.upgrade().is_none());
        assert!(!highlight::is_cached(&id, "c.rs"), "nothing was highlighted for a closed diff");

        // Still open: the same job does the work.
        let slots = std::sync::Arc::new(vec![std::sync::OnceLock::new()]);
        Job { repo: dir.clone(), slots: slots.clone(), files: vec![(0, file())] }.run(&|| {});
        assert!(slots[0].get().unwrap().old.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linked_worktrees_read_their_own_files_for_colours_and_sizes() {
        use crate::app::DiffDoc;
        use crate::highlight::Tok;
        use crate::views::DiffMode;
        let dir = temp_repo("hl-worktree");
        std::fs::write(dir.join("lib.rs"), "fn a() {}\nconst S: &str = \"x\";\n").unwrap();
        std::fs::write(dir.join("data.bin"), vec![0u8; 1000]).unwrap();
        stage(&dir, &["lib.rs".into(), "data.bin".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        let wt = dir.with_file_name(format!("{}-wt", dir.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_dir_all(&wt);
        run(&dir, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]).unwrap();
        assert!(wt.join(".git").is_file(), "a linked worktree's .git is a file, not a folder");

        // Changed only in the worktree; the main checkout keeps the old files.
        std::fs::write(wt.join("lib.rs"), "fn a() {}\nconst S: &str = \"changed in the worktree\";\n").unwrap();
        std::fs::write(wt.join("data.bin"), vec![1u8; 4000]).unwrap();
        let st = status(&wt).unwrap();
        let (unstaged, _) = worktree_diffs(&wt, &st, false).unwrap();
        let mut doc = DiffDoc::working(vec![("UNSTAGED".into(), DiffMode::Unstaged, unstaged.into_iter().map(|f| (f, false)).collect())]);
        doc.load_binaries(&wt, None);
        let bin = doc.files.iter().position(|f| f.path == "data.bin").unwrap();
        assert_eq!(doc.binary_sizes.get(&bin), Some(&(Some(1000), Some(4000))), "the new size is the worktree's file");

        doc.highlighter(&wt).run(&|| {});
        let rs = doc.files.iter().position(|f| f.path == "lib.rs").unwrap();
        let added = doc.files[rs].hunks[0].lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
        assert_eq!(added.text, "const S: &str = \"changed in the worktree\";");
        let toks: Vec<Tok> = doc.spans(rs, added).expect("coloured from the worktree's copy").iter().map(|s| s.tok).collect();
        assert!(toks.contains(&Tok::String));
        assert!(doc.spans(rs, doc.files[rs].hunks[0].lines.iter().find(|l| l.kind == LineKind::Del).unwrap()).is_some(), "and the old side from git");
        let _ = run(&dir, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renamed_files_use_their_old_name_for_the_old_side() {
        use crate::app::DiffDoc;
        use crate::highlight::Tok;
        use crate::views::DiffMode;
        let dir = temp_repo("hl-rename");
        // Python, renamed to a name Headway doesn't know: "# ..." is a comment only in the old one.
        let body: String = (1..=12).map(|i| format!("value_{i} = {i}\n")).collect();
        std::fs::write(dir.join("tool.py"), format!("# settings\n{body}")).unwrap();
        std::fs::write(dir.join("blob.bin"), [vec![0u8; 3000], vec![7u8; 100]].concat()).unwrap();
        std::fs::write(dir.join("tidy.rs"), "fn a() {\n    b();\n    c();\n    d();\n}\n").unwrap();
        stage(&dir, &["tool.py".into(), "blob.bin".into(), "tidy.rs".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        run(&dir, &["mv", "tool.py", "tool"]).unwrap();
        std::fs::write(dir.join("tool"), format!("# settings\n{body}value_13 = 13\n")).unwrap();
        run(&dir, &["mv", "blob.bin", "blob2.bin"]).unwrap();
        std::fs::write(dir.join("blob2.bin"), [vec![0u8; 3000], vec![7u8; 120]].concat()).unwrap();
        stage(&dir, &["tool".into(), "blob2.bin".into()]).unwrap();

        let st = status(&dir).unwrap();
        let (_, staged) = worktree_diffs(&dir, &st, false).unwrap();
        let mut doc = DiffDoc::working(vec![("STAGED".into(), DiffMode::Staged, staged.into_iter().map(|f| (f, false)).collect())]);
        let tool = doc.files.iter().position(|f| f.path == "tool").unwrap();
        assert_eq!(doc.files[tool].old_path.as_deref(), Some("tool.py"));
        doc.highlighter(&dir).run(&|| {});
        let sides = doc.syntax[tool].get().expect("still coloured: the old name is Python");
        let first_old = sides.old.as_ref().unwrap().line(1, "# settings").unwrap();
        assert_eq!(first_old.iter().map(|s| s.tok).collect::<Vec<_>>(), [Tok::Comment]);
        assert!(sides.new.is_none(), "the new name isn't a known language");

        // A renamed binary file gets both sizes.
        doc.load_binaries(&dir, None);
        let bin = doc.files.iter().position(|f| f.path == "blob2.bin").unwrap();
        assert_eq!(doc.files[bin].old_path.as_deref(), Some("blob.bin"));
        assert_eq!(doc.binary_sizes.get(&bin), Some(&(Some(3100), Some(3120))));

        // Renamed and only re-indented: with whitespace ignored it's still listed, under both names.
        commit(&dir, "rename", false).unwrap();
        run(&dir, &["mv", "tidy.rs", "neat.rs"]).unwrap();
        std::fs::write(dir.join("neat.rs"), "fn a() {\n\tb();\n    c();\n    d();\n}\n").unwrap();
        stage(&dir, &["neat.rs".into()]).unwrap();
        commit(&dir, "move and re-indent", false).unwrap();
        let head = run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let d = commit_detail(&dir, &head, true).unwrap();
        assert_eq!(d.files.len(), 1, "{:?}", d.files.iter().map(|f| &f.path).collect::<Vec<_>>());
        let f = &d.files[0];
        assert_eq!((f.path.as_str(), f.old_path.as_deref(), f.change.clone()), ("neat.rs", Some("tidy.rs"), Change::Renamed));
        assert_eq!(f.note.as_deref(), Some(WHITESPACE_ONLY));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignoring_whitespace_hides_whitespace_only_changes() {
        let dir = temp_repo("ignore-ws");
        std::fs::write(dir.join("a.rs"), "fn a() {\n    x();\n}\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        stage(&dir, &["a.rs".into(), "b.rs".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        // a.rs: only re-indented; b.rs: a real change.
        std::fs::write(dir.join("a.rs"), "fn a() {\n\tx();\n}\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn b() { y(); }\n").unwrap();
        let st = status(&dir).unwrap();

        let (shown, _) = worktree_diffs(&dir, &st, false).unwrap();
        assert!(shown.iter().all(|f| f.hunks.len() == 1 && f.note.is_none()));
        let (ignored, _) = worktree_diffs(&dir, &st, true).unwrap();
        let a = ignored.iter().find(|f| f.path == "a.rs").unwrap();
        assert!(a.hunks.is_empty());
        assert_eq!(a.note.as_deref(), Some(WHITESPACE_ONLY));
        assert_eq!(ignored.iter().find(|f| f.path == "b.rs").unwrap().hunks.len(), 1, "real changes still show");

        // The same for a commit and for a file's history.
        stage(&dir, &["a.rs".into(), "b.rs".into()]).unwrap();
        commit(&dir, "second", false).unwrap();
        let head = run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let d = commit_detail(&dir, &head, true).unwrap();
        assert_eq!(d.files.iter().find(|f| f.path == "a.rs").and_then(|f| f.note.as_deref()), Some(WHITESPACE_ONLY));
        assert_eq!(commit_detail(&dir, &head, false).unwrap().files.iter().find(|f| f.path == "a.rs").unwrap().hunks.len(), 1);
        let files = commit_files(&dir, &head, &["a.rs".into()], true).unwrap();
        assert_eq!(files[0].note.as_deref(), Some(WHITESPACE_ONLY));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunk_stage_unstage_discard_and_commit() {
        let dir = temp_repo("hunks");
        let original: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("f.txt"), &original).unwrap();
        stage(&dir, &["f.txt".into()]).unwrap();
        commit(&dir, "first", false).unwrap();

        // Two separate edits -> two hunks.
        let edited = original.replace("line 2\n", "line two\n").replace("line 28\n", "line twenty-eight\n");
        std::fs::write(dir.join("f.txt"), &edited).unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert_eq!(d.hunks.len(), 2);

        // Stage only the first hunk.
        apply_hunk(&dir, &d.hunk_patch(0).unwrap(), false, true).unwrap();
        let staged = file_diff(&dir, "f.txt", true, false).unwrap().unwrap();
        let unstaged = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert_eq!(staged.hunks.len(), 1);
        assert_eq!(unstaged.hunks.len(), 1);
        let st = status(&dir).unwrap();
        assert_eq!(st.staged.len(), 1);
        assert_eq!(st.unstaged.len(), 1);

        // Unstage it again.
        apply_hunk(&dir, &staged.hunk_patch(0).unwrap(), true, true).unwrap();
        assert!(file_diff(&dir, "f.txt", true, false).unwrap().is_none());

        // Discard the second hunk from the working file.
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        apply_hunk(&dir, &d.hunk_patch(1).unwrap(), true, false).unwrap();
        let now = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        assert!(now.contains("line two") && now.contains("line 28\n"));

        // Commit everything, then amend.
        stage(&dir, &["f.txt".into()]).unwrap();
        commit(&dir, "second\n\nwith body", false).unwrap();
        commit(&dir, "second, amended", true).unwrap();
        let commits = log(&dir, 100).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].summary, "second, amended");
        assert!(commits[0].refs.iter().any(|r| r.kind == RefKind::Head && r.name == "main"));
        let detail = commit_detail(&dir, &commits[0].hash, false).unwrap();
        assert_eq!(detail.files.len(), 1);
        assert_eq!(detail.files[0].added, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn changed_lines(f: &DiffFile, pick: impl Fn(&DiffLine) -> bool) -> LineSelection {
        let mut sel = LineSelection::new();
        for (hi, h) in f.hunks.iter().enumerate() {
            for (li, l) in h.lines.iter().enumerate() {
                if matches!(l.kind, LineKind::Add | LineKind::Del) && pick(l) {
                    sel.insert((hi, li));
                }
            }
        }
        sel
    }

    fn index_text(dir: &Path, path: &str) -> String {
        run(dir, &["show", &format!(":{path}")]).unwrap()
    }

    #[test]
    fn stage_unstage_and_discard_single_lines() {
        let dir = temp_repo("lines");
        let original: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join("f.txt"), &original).unwrap();
        stage(&dir, &["f.txt".into()]).unwrap();
        commit(&dir, "first", false).unwrap();

        // One hunk: replace line 3, add two lines after 4, remove line 6.
        let edited = original
            .replace("line 3\n", "line THREE\n")
            .replace("line 4\n", "line 4\nnew a\nnew b\n")
            .replace("line 6\n", "");
        std::fs::write(dir.join("f.txt"), &edited).unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert_eq!(d.hunks.len(), 1);

        // Stage only "new a" and the removal of line 6.
        let sel = changed_lines(&d, |l| l.text == "new a" || l.text == "line 6");
        stage_lines(&dir, &d, &sel, false).unwrap();
        let idx = index_text(&dir, "f.txt");
        assert!(idx.contains("line 3\n") && !idx.contains("THREE"));
        assert!(idx.contains("new a\n") && !idx.contains("new b"));
        assert!(!idx.contains("line 6\n"));
        // The working file is untouched.
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), edited);

        // Unstage just "new a" again.
        let staged = file_diff(&dir, "f.txt", true, false).unwrap().unwrap();
        let sel = changed_lines(&staged, |l| l.text == "new a");
        unstage_lines(&dir, &staged, &sel).unwrap();
        let idx = index_text(&dir, "f.txt");
        assert!(!idx.contains("new a") && !idx.contains("line 6\n"));

        // Discard "new b" from the working file only.
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        let sel = changed_lines(&d, |l| l.text == "new b");
        discard_lines(&dir, &d, &sel).unwrap();
        let disk = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        assert!(disk.contains("new a\n") && !disk.contains("new b") && disk.contains("THREE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_lines_of_new_files() {
        let dir = temp_repo("newlines");
        std::fs::write(dir.join("base.txt"), "x\n").unwrap();
        stage(&dir, &["base.txt".into()]).unwrap();
        commit(&dir, "base", false).unwrap();

        // Untracked file: stage two of three lines.
        std::fs::write(dir.join("n.txt"), "one\ntwo\nthree\n").unwrap();
        let d = file_diff(&dir, "n.txt", false, true).unwrap().unwrap();
        let sel = changed_lines(&d, |l| l.text != "two");
        stage_lines(&dir, &d, &sel, true).unwrap();
        assert_eq!(index_text(&dir, "n.txt"), "one\nthree\n");

        // Staged new file: unstage one line (the file stays added).
        let staged = file_diff(&dir, "n.txt", true, false).unwrap().unwrap();
        let sel = changed_lines(&staged, |l| l.text == "one");
        unstage_lines(&dir, &staged, &sel).unwrap();
        assert_eq!(index_text(&dir, "n.txt"), "three\n");

        // A file without a final newline.
        std::fs::write(dir.join("base.txt"), "x\ny").unwrap();
        let d = file_diff(&dir, "base.txt", false, false).unwrap().unwrap();
        let sel = changed_lines(&d, |l| l.text == "y");
        stage_lines(&dir, &d, &sel, false).unwrap();
        assert_eq!(index_text(&dir, "base.txt"), "x\ny");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_untracked_rename_and_unstage_without_head() {
        let dir = temp_repo("status");
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        // No commit yet: unstaging must still work.
        unstage(&dir, &["a.txt".into()]).unwrap();
        let st = status(&dir).unwrap();
        assert_eq!(st.unstaged[0].change, Change::Untracked);
        assert!(log(&dir, 10).unwrap().is_empty());

        stage(&dir, &["a.txt".into()]).unwrap();
        commit(&dir, "add a", false).unwrap();
        run(&dir, &["mv", "a.txt", "b.txt"]).unwrap();
        std::fs::write(dir.join("new file.txt"), "x\n").unwrap();
        let st = status(&dir).unwrap();
        assert_eq!(st.head.branch.as_deref(), Some("main"));
        assert_eq!(st.staged[0].change, Change::Renamed);
        assert_eq!(st.staged[0].path, "b.txt");
        assert_eq!(st.staged[0].old_path.as_deref(), Some("a.txt"));
        assert!(st.unstaged.iter().any(|f| f.path == "new file.txt" && f.change == Change::Untracked));
        let d = file_diff(&dir, "new file.txt", false, true).unwrap().unwrap();
        assert_eq!(d.added, 1);

        // Discarding an untracked file deletes it.
        discard(&dir, &st.unstaged.iter().find(|f| f.path == "new file.txt").unwrap().clone()).unwrap();
        assert!(!dir.join("new file.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn graph_lanes_for_merge() {
        let dir = temp_repo("graph");
        let c = |m: &str| {
            std::fs::write(dir.join(m), m).unwrap();
            stage(&dir, &[m.into()]).unwrap();
            commit(&dir, m, false).unwrap();
        };
        c("base");
        run(&dir, &["checkout", "-q", "-b", "side"]).unwrap();
        c("side1");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        c("main1");
        run(&dir, &["merge", "-q", "--no-edit", "side"]).unwrap();
        let commits = log(&dir, 100).unwrap();
        let g = crate::graph::layout(&commits);
        assert_eq!(commits.len(), 4);
        assert_eq!(g.width, 2);
        assert_eq!(g.rows[0].to_parents.len(), 2);
        // Every line ends at the root commit.
        let last = g.rows.last().unwrap();
        assert!(!last.top.is_empty() && last.to_parents.is_empty());
        let (branches, _) = branches(&dir).unwrap();
        assert_eq!(branches.iter().filter(|b| !b.remote).count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_addresses_and_names() {
        let out = "origin\thttps://example.com/a.git (fetch)\norigin\thttps://example.com/a.git (push)\nfork\tgit@host:me/a.git (fetch)\nfork\tgit@host:me/a.git (push)\n";
        assert_eq!(
            parse_remote_urls(out),
            [("origin".to_string(), "https://example.com/a.git".to_string()), ("fork".to_string(), "git@host:me/a.git".to_string())]
        );
        for ok in ["origin", "upstream", "my-fork", "team_2", "a.b"] {
            assert!(valid_remote_name(ok), "{ok}");
        }
        for bad in ["", " ", "has space", "-x", ".hidden", "a/b", "x.lock", "a:b"] {
            assert!(!valid_remote_name(bad), "{bad}");
        }
    }

    #[test]
    fn add_fetch_and_remove_remotes() {
        let up = temp_repo("remote-up");
        std::fs::write(up.join("f.txt"), "1\n").unwrap();
        stage(&up, &["f.txt".into()]).unwrap();
        commit(&up, "first", false).unwrap();
        let dir = temp_repo("remote-down");
        add_remote(&dir, "upstream", up.to_str().unwrap(), true).unwrap();
        assert_eq!(remotes(&dir).unwrap(), ["upstream"]);
        assert_eq!(remote_urls(&dir).unwrap()[0].1, up.to_str().unwrap());
        let (fetched, _) = branches(&dir).unwrap();
        assert!(fetched.iter().any(|b| b.remote && b.name == "upstream/main"), "fetched");
        // Adding the same name twice fails; a bad address is added but the fetch fails.
        assert!(add_remote(&dir, "upstream", "x", false).is_err());
        let e = add_remote(&dir, "broken", "/no/such/repo", true).unwrap_err();
        assert!(e.contains("was added, but fetching it failed"));
        remove_remote(&dir, "broken").unwrap();
        remove_remote(&dir, "upstream").unwrap();
        assert!(remotes(&dir).unwrap().is_empty());
        assert!(branches(&dir).unwrap().0.iter().all(|b| !b.remote), "its branches go too");
        let _ = std::fs::remove_dir_all(&up);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_branches_leave_the_history() {
        let dir = temp_repo("hide");
        let c = |m: &str| {
            std::fs::write(dir.join(m), m).unwrap();
            stage(&dir, &[m.into()]).unwrap();
            commit(&dir, m, false).unwrap();
        };
        c("base");
        run(&dir, &["checkout", "-q", "-b", "fix/side"]).unwrap();
        c("side work");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        c("main work");
        let all = log(&dir, 50).unwrap();
        assert_eq!(all.len(), 3);
        let hidden = vec!["refs/heads/fix/side".to_string()];
        let some = log_filtered(&dir, 50, &hidden).unwrap();
        let summaries: Vec<&str> = some.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["main work", "base"], "the side branch's own commit is gone");
        assert!(some.iter().all(|c| c.refs.iter().all(|r| r.name != "fix/side")));
        // A hidden branch pointing at a shared commit only loses its label.
        run(&dir, &["branch", "marker", "HEAD~1"]).unwrap();
        let some = log_filtered(&dir, 50, &["refs/heads/marker".to_string()]).unwrap();
        assert_eq!(some.len(), 3);
        assert!(some.iter().all(|c| c.refs.iter().all(|r| r.name != "marker")));
        assert_eq!(label_ref(&RefLabel { name: "origin/x".into(), kind: RefKind::Remote }).as_deref(), Some("refs/remotes/origin/x"));
        assert_eq!(label_ref(&RefLabel { name: "HEAD".into(), kind: RefKind::Head }), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_submodule_status() {
        let status = " 1111111111111111111111111111111111111111 libs/ui (v1.2-3-g1111111)
-2222222222222222222222222222222222222222 vendor/old lib
+3333333333333333333333333333333333333333 tools (heads/main)
U4444444444444444444444444444444444444444 broken
";
        let config = "submodule.ui.path libs/ui
submodule.ui.url https://example.com/ui.git
submodule.old.path vendor/old lib
submodule.old.url git@host:old.git
";
        let s = parse_submodules(status, config);
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].name.as_str(), s[0].path.as_str(), s[0].state), ("ui", "libs/ui", SubState::Ready));
        assert_eq!(s[0].url.as_deref(), Some("https://example.com/ui.git"));
        assert_eq!((s[1].path.as_str(), s[1].state), ("vendor/old lib", SubState::NotSetUp), "paths with spaces, no describe");
        assert_eq!(s[1].url.as_deref(), Some("git@host:old.git"));
        assert_eq!((s[2].path.as_str(), s[2].state, s[2].url.as_deref()), ("tools", SubState::Moved, None));
        assert_eq!(s[3].state, SubState::Conflict);
        assert_eq!(s[2].sha, "3333333333333333333333333333333333333333");
        assert_eq!(submodule_folder("https://example.com/team/ui-kit.git/"), "ui-kit");
        assert_eq!(submodule_folder("git@host:team/tools.git"), "tools");
    }

    #[test]
    fn add_update_and_remove_submodules() {
        // Local paths are blocked for submodules by default; allow them for this test only.
        // SAFETY: only adds a git setting that every test can live with.
        unsafe { std::env::set_var("GIT_CONFIG_PARAMETERS", "'protocol.file.allow=always'") };
        let lib = temp_repo("sub-lib");
        std::fs::write(lib.join("lib.txt"), "v1\n").unwrap();
        stage(&lib, &["lib.txt".into()]).unwrap();
        commit(&lib, "lib v1", false).unwrap();

        let app = temp_repo("sub-app");
        std::fs::write(app.join("app.txt"), "app\n").unwrap();
        stage(&app, &["app.txt".into()]).unwrap();
        commit(&app, "app", false).unwrap();
        assert!(submodules(&app).unwrap().is_empty());

        add_submodule(&app, lib.to_str().unwrap(), "libs/core").unwrap();
        commit(&app, "add lib", false).unwrap();
        let subs = submodules(&app).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!((subs[0].path.as_str(), subs[0].state), ("libs/core", SubState::Ready));
        assert_eq!(subs[0].url.as_deref(), Some(lib.to_str().unwrap()));
        assert!(is_repo(&app.join("libs/core")));

        // A new commit upstream: "latest" moves the submodule; the parent sees it as changed.
        std::fs::write(lib.join("lib.txt"), "v2\n").unwrap();
        stage(&lib, &["lib.txt".into()]).unwrap();
        commit(&lib, "lib v2", false).unwrap();
        update_submodules(&app, Some("libs/core"), true).unwrap();
        let st = status(&app).unwrap();
        let entry = st.unstaged.iter().find(|f| f.path == "libs/core").unwrap();
        assert!(entry.submodule, "{st:?}");
        assert_eq!(submodules(&app).unwrap()[0].state, SubState::Moved);
        // "Discard" puts it back on the recorded commit…
        discard(&app, entry).unwrap();
        assert!(status(&app).unwrap().unstaged.is_empty());
        // …and "latest" moves it forward again.
        update_submodules(&app, Some("libs/core"), true).unwrap();
        assert!(!st.unstaged.iter().any(|f| f.path == "app.txt" && f.submodule));
        // The diff of a submodule names the two commits.
        let d = file_diff(&app, "libs/core", false, false).unwrap().unwrap();
        assert!(d.hunks[0].lines.iter().any(|l| l.text.starts_with("Subproject commit")));
        // Back to the recorded commit.
        update_submodules(&app, None, false).unwrap();
        assert!(status(&app).unwrap().unstaged.is_empty());

        remove_submodule(&app, &subs[0]).unwrap();
        commit(&app, "remove lib", false).unwrap();
        assert!(submodules(&app).unwrap().is_empty());
        assert!(!app.join("libs/core").exists());
        let _ = std::fs::remove_dir_all(&lib);
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn every_uncommitted_change_in_status_order() {
        let dir = temp_repo("wdiff");
        std::fs::write(dir.join("a.txt"), "1\n").unwrap();
        std::fs::write(dir.join("b.txt"), "1\n").unwrap();
        stage(&dir, &["a.txt".into(), "b.txt".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        std::fs::write(dir.join("b.txt"), "2\n").unwrap();
        stage(&dir, &["b.txt".into()]).unwrap();
        std::fs::write(dir.join("b.txt"), "3\n").unwrap();
        std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
        std::fs::write(dir.join("big.bin"), vec![b'x'; (MAX_NEW_FILE_BYTES + 1) as usize]).unwrap();
        let st = status(&dir).unwrap();
        let (unstaged, staged) = worktree_diffs(&dir, &st, false).unwrap();
        let names: Vec<&str> = unstaged.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, st.unstaged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>());
        assert!(names.contains(&"a.txt") && names.contains(&"b.txt") && names.contains(&"new.txt"));
        let new = unstaged.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!((new.change.clone(), new.added), (Change::Untracked, 1));
        let big = unstaged.iter().find(|f| f.path == "big.bin").unwrap();
        assert!(big.note.as_deref().unwrap().contains("Large"));
        // b.txt is both staged (1→2) and changed again (2→3).
        assert_eq!(staged.len(), 1);
        assert!(staged[0].hunks[0].lines.iter().any(|l| l.text == "2" && l.kind == LineKind::Add));
        let b = unstaged.iter().find(|f| f.path == "b.txt").unwrap();
        assert!(b.hunks[0].lines.iter().any(|l| l.text == "3" && l.kind == LineKind::Add));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn commit_file_with(dir: &Path, name: &str, text: &str, msg: &str) {
        std::fs::write(dir.join(name), text).unwrap();
        stage(dir, &[name.into()]).unwrap();
        commit(dir, msg, false).unwrap();
    }

    fn message_of(dir: &Path, rev: &str) -> String {
        run(dir, &["log", "-1", "--format=%B", rev]).unwrap().trim_end().to_string()
    }

    #[test]
    fn reword_the_latest_commit_keeps_staged_work() {
        let dir = temp_repo("reword-head");
        commit_file_with(&dir, "a.txt", "1\n", "Frist commit");
        std::fs::write(dir.join("b.txt"), "staged\n").unwrap();
        stage(&dir, &["b.txt".into()]).unwrap();
        let head = run(&dir, &["rev-parse", "HEAD"]).unwrap();
        let new = reword(&dir, head.trim(), "First commit\n\nWith details.").unwrap();
        assert_eq!(message_of(&dir, "HEAD"), "First commit\n\nWith details.");
        assert_eq!(new, run(&dir, &["rev-parse", "HEAD"]).unwrap().trim());
        // The staged file wasn't swept into the commit.
        let st = status(&dir).unwrap();
        assert!(st.staged.iter().any(|f| f.path == "b.txt"));
        assert!(run(&dir, &["show", "--name-only", "--format=", "HEAD"]).unwrap().trim() == "a.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_an_older_commit_replays_the_rest() {
        let dir = temp_repo("reword-old");
        commit_file_with(&dir, "a.txt", "1\n", "one");
        commit_file_with(&dir, "b.txt", "2\n", "tow");
        commit_file_with(&dir, "c.txt", "3\n", "three");
        run(&dir, &["checkout", "-q", "-b", "side", "HEAD~1"]).unwrap();
        commit_file_with(&dir, "d.txt", "4\n", "side work");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        run(&dir, &["merge", "-q", "--no-edit", "side"]).unwrap();
        commit_file_with(&dir, "e.txt", "5\n", "five");
        std::fs::write(dir.join("a.txt"), "uncommitted\n").unwrap();

        let tree_before = run(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let old = run(&dir, &["log", "--format=%H", "--grep=^tow$"]).unwrap();
        let author_before = run(&dir, &["log", "-1", "--format=%an %ae %ad", old.trim()]).unwrap();
        let new = reword(&dir, old.trim(), "two").unwrap();

        assert_eq!(message_of(&dir, &new), "two");
        assert_eq!(run(&dir, &["log", "-1", "--format=%an %ae %ad", &new]).unwrap(), author_before, "author kept");
        assert_eq!(run(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap(), tree_before, "same files at the end");
        let subjects = run(&dir, &["log", "--format=%s"]).unwrap();
        assert!(subjects.contains("two") && !subjects.contains("tow"));
        for s in ["one", "three", "side work", "five", "Merge branch 'side'"] {
            assert!(subjects.contains(s), "{s} survived:\n{subjects}");
        }
        run(&dir, &["merge-base", "--is-ancestor", &new, "HEAD"]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "uncommitted\n", "uncommitted work kept");
        assert_eq!(status(&dir).unwrap().head.branch.as_deref(), Some("main"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_works_on_other_branches_but_not_on_remote_only_commits() {
        let dir = temp_repo("reword-off");
        commit_file_with(&dir, "a.txt", "1\n", "one");
        run(&dir, &["checkout", "-q", "-b", "other"]).unwrap();
        commit_file_with(&dir, "b.txt", "2\n", "elswhere");
        let other = run(&dir, &["rev-parse", "HEAD"]).unwrap();
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        let main_before = run(&dir, &["rev-parse", "main"]).unwrap();
        let new = reword(&dir, other.trim(), "elsewhere").unwrap();
        assert_eq!(message_of(&dir, "other"), "elsewhere");
        assert_eq!(run(&dir, &["rev-parse", "other"]).unwrap().trim(), new);
        assert_eq!(run(&dir, &["rev-parse", "main"]).unwrap(), main_before, "main is untouched");
        // A commit only a remote branch has can't be edited.
        run(&dir, &["update-ref", "refs/remotes/origin/x", other.trim()]).unwrap();
        let e = reword(&dir, other.trim(), "x").unwrap_err();
        assert!(e.contains("isn't on any of your branches"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Everything about a repository that editing a message must not change (besides ids).
    #[derive(Debug, PartialEq)]
    struct Snapshot {
        /// Every commit reachable from local branches/tags: (subject, author+date, committer+date, tree).
        commits: Vec<String>,
        /// Branch/tag -> (tree it points at, subject).
        refs: Vec<String>,
        head: String,
        status: String,
        files: Vec<(String, String)>,
    }

    fn snapshot(dir: &Path) -> Snapshot {
        let mut commits: Vec<String> = run(dir, &["log", "--branches", "--tags", "--format=%s|%an %ae %ad|%cn %ce %cd|%T"])
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        commits.sort();
        let refs = run(dir, &["for-each-ref", "--format=%(refname) %(*objecttype)%(objecttype) %(contents:subject)", "refs/heads", "refs/tags"])
            .unwrap()
            .lines()
            .map(|l| {
                let name = l.split(' ').next().unwrap();
                let tree = run(dir, &["rev-parse", &format!("{name}^{{tree}}")]).unwrap();
                format!("{l} -> {}", tree.trim())
            })
            .collect();
        let head = run(dir, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_else(|_| "detached".into());
        let status = run(dir, &["status", "--porcelain=v1"]).unwrap();
        let mut files: Vec<(String, String)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read_to_string(e.path()).unwrap_or_default()))
            .collect();
        files.sort();
        Snapshot { commits, refs, head, status, files }
    }

    /// The person's case: an old commit under a merged feature branch, an unmerged
    /// branch, a tag and an annotated tag, plus staged and unstaged work.
    fn shop(dir: &Path) {
        let c = |f: &str, t: &str, m: &str| commit_file_with(dir, f, t, m);
        c("index.html", "<h1>Shop</h1>\n", "Initial storefront skeleton");
        c("style.css", "body{}\n", "Add base stylesheet");
        c("cart.js", "total\n", "Add cart total calculation");
        run(dir, &["tag", "-a", "v0.9", "-m", "Beta release"]).unwrap();
        run(dir, &["checkout", "-q", "-b", "feature/checkout"]).unwrap();
        c("checkout.js", "checkout\n", "Start checkout flow");
        c("checkout.js", "checkout coupon\n", "Support coupon codes at checkout");
        run(dir, &["checkout", "-q", "main"]).unwrap();
        c("style.css", "body{font:sans}\n", "Refine typography");
        run(dir, &["merge", "-q", "--no-ff", "--no-edit", "feature/checkout"]).unwrap();
        run(dir, &["tag", "v1.0.0"]).unwrap();
        c("index.html", "<h1>Shop</h1><div id=cart>\n", "Link stylesheet and add cart container");
        run(dir, &["checkout", "-q", "-b", "feature/search"]).unwrap();
        c("search.js", "search\n", "Add product search");
        c("search.js", "search trim\n", "Ignore empty search queries");
        run(dir, &["checkout", "-q", "main"]).unwrap();
        c("README.md", "run it\n", "Document how to run the shop");
        c("cart.js", "total qty\n", "Count quantities in cart total");
        std::fs::write(dir.join("style.css"), "body{font:serif}\n").unwrap();
        stage(dir, &["style.css".into()]).unwrap();
        std::fs::write(dir.join("cart.js"), "total qty edited\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "new file\n").unwrap();
    }

    fn commit_id(dir: &Path, subject: &str) -> String {
        run(dir, &["log", "--all", "--format=%H", &format!("--grep=^{subject}$")]).unwrap().trim().to_string()
    }

    /// Check that editing `subject` changes exactly that message and nothing else.
    fn assert_clean_reword(dir: &Path, subject: &str, new_message: &str) {
        let before = snapshot(dir);
        let old_ids: Vec<String> = run(dir, &["rev-list", "--branches", "--tags"]).unwrap().lines().map(String::from).collect();
        let target = commit_id(dir, subject);
        let new = reword(dir, &target, new_message).unwrap();
        let after = snapshot(dir);

        // Exactly one message changed; every other detail of every commit is the same.
        let new_subject = new_message.lines().next().unwrap_or("");
        let expected: Vec<String> = {
            let mut v: Vec<String> = before.commits.iter().map(|c| if c.starts_with(&format!("{subject}|")) { c.replacen(subject, new_subject, 1) } else { c.clone() }).collect();
            v.sort();
            v
        };
        assert_eq!(after.commits, expected, "same commits, no duplicates, only one message changed");
        assert_eq!(after.commits.len(), before.commits.len());
        // Branches and tags still point at the same files.
        let strip = |r: &[String]| -> Vec<String> { r.iter().map(|l| l.replacen(subject, new_subject, 1)).collect() };
        assert_eq!(after.refs, strip(&before.refs));
        assert_eq!((after.head.clone(), after.status.clone(), after.files.clone()), (before.head.clone(), before.status.clone(), before.files.clone()));
        assert_eq!(message_of(dir, &new), new_message);
        // The original commit and everything after it are gone from your branches and tags.
        let now: std::collections::HashSet<String> = run(dir, &["rev-list", "--branches", "--tags"]).unwrap().lines().map(String::from).collect();
        assert!(!now.contains(&target));
        let kept = old_ids.iter().filter(|i| now.contains(*i)).count();
        let descendants = old_ids.iter().filter(|i| run(dir, &["merge-base", "--is-ancestor", &target, i]).is_ok()).count();
        assert_eq!(kept, old_ids.len() - descendants, "only the edited commit and later ones got new ids");
    }

    #[test]
    fn reword_the_first_commit_moves_every_branch_and_tag() {
        let dir = temp_repo("reword-shop-root");
        shop(&dir);
        assert_clean_reword(&dir, "Initial storefront skeleton", "Initial storefront");
        // The annotated tag kept its own message and tagger.
        let tag = run(&dir, &["cat-file", "tag", "v0.9"]).unwrap();
        assert!(tag.contains("Beta release") && tag.contains("tagger Test <t@example.com>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_commits_all_over_the_history() {
        // Each kind of commit: inside a merged branch, the merge itself, on an unmerged
        // branch, the latest one, and one a tag points at.
        for subject in [
            "Start checkout flow",
            "Merge branch 'feature/checkout'",
            "Add product search",
            "Ignore empty search queries",
            "Count quantities in cart total",
            "Add cart total calculation",
            "Refine typography",
        ] {
            let dir = temp_repo(&format!("reword-shop-{}", subject.len()));
            shop(&dir);
            assert_clean_reword(&dir, subject, &format!("{subject} (edited)\n\nWith a longer explanation."));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn reword_twice_in_a_row() {
        let dir = temp_repo("reword-twice");
        shop(&dir);
        assert_clean_reword(&dir, "Add base stylesheet", "Add the base stylesheet");
        assert_clean_reword(&dir, "Refine typography", "Refine the typography");
        assert_clean_reword(&dir, "Add the base stylesheet", "Add base styles");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_with_detached_head_and_another_worktree() {
        let dir = temp_repo("reword-wt");
        shop(&dir);
        // Commit the work so the checkout below is clean.
        stage(&dir, &["cart.js".into(), "notes.txt".into()]).unwrap();
        commit(&dir, "Work in progress", false).unwrap();
        let wt = dir.with_file_name(format!("{}-search", repo_name(&dir)));
        let _ = std::fs::remove_dir_all(&wt);
        add_worktree(&dir, &wt, "feature/search", None).unwrap();
        std::fs::write(wt.join("search.js"), "uncommitted in worktree\n").unwrap();
        run(&dir, &["checkout", "-q", "--detach", "HEAD~1"]).unwrap();
        let before_head_tree = run(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap();

        let target = commit_id(&dir, "Add base stylesheet");
        reword(&dir, &target, "Add the base stylesheet").unwrap();

        // Detached HEAD followed along (and stayed detached, same files).
        assert!(run(&dir, &["symbolic-ref", "-q", "HEAD"]).is_err());
        assert_eq!(run(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap(), before_head_tree);
        run(&dir, &["merge-base", "--is-ancestor", "main", "HEAD"]).unwrap_err();
        assert!(run(&dir, &["log", "--format=%s", "HEAD"]).unwrap().contains("Add the base stylesheet"));
        // The other worktree's branch moved and its uncommitted change is still there.
        assert!(run(&wt, &["log", "--format=%s"]).unwrap().contains("Add the base stylesheet"));
        assert_eq!(std::fs::read_to_string(wt.join("search.js")).unwrap(), "uncommitted in worktree\n");
        let st = status(&wt).unwrap();
        assert_eq!(st.unstaged.len(), 1);
        assert!(st.staged.is_empty());
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_keeps_remote_branches_on_the_old_commits() {
        let up = temp_repo("reword-up");
        shop(&up);
        let down = std::env::temp_dir().join(format!("headway-test-reword-down-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&down);
        run(&std::env::temp_dir(), &["clone", "-q", up.to_str().unwrap(), down.to_str().unwrap()]).unwrap();
        let origin_before = run(&down, &["rev-parse", "origin/main"]).unwrap();
        let target = commit_id(&down, "Refine typography");
        reword(&down, &target, "Refine the typography").unwrap();
        assert_eq!(run(&down, &["rev-parse", "origin/main"]).unwrap(), origin_before, "the server's branch isn't touched");
        assert!(run(&down, &["log", "--format=%s", "main"]).unwrap().contains("Refine the typography"));
        let _ = std::fs::remove_dir_all(&up);
        let _ = std::fs::remove_dir_all(&down);
    }

    #[test]
    fn reword_drops_signatures_that_would_no_longer_match() {
        let dir = temp_repo("reword-sig");
        commit_file_with(&dir, "a.txt", "1\n", "one");
        // A commit with a (fake) signature header, and a signed annotated tag on it.
        let parent = run(&dir, &["rev-parse", "HEAD"]).unwrap();
        let tree = run(&dir, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let raw = format!(
            "tree {}\nparent {}\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n\nsigned work\n",
            tree.trim(),
            parent.trim()
        );
        let signed = write_object(&dir, "commit", &raw).unwrap();
        run(&dir, &["update-ref", "refs/heads/main", &signed]).unwrap();
        run(&dir, &["reset", "-q", "--hard"]).unwrap();
        let tag = format!(
            "object {signed}\ntype commit\ntag v1\ntagger A <a@x> 1700000000 +0000\n\nRelease one\n-----BEGIN PGP SIGNATURE-----\nxyz\n-----END PGP SIGNATURE-----\n"
        );
        let tag_id = write_object(&dir, "tag", &tag).unwrap();
        run(&dir, &["update-ref", "refs/tags/v1", &tag_id]).unwrap();

        reword(&dir, parent.trim(), "the first one").unwrap();
        let new_child = run(&dir, &["cat-file", "commit", "main"]).unwrap();
        assert!(!new_child.contains("gpgsig") && !new_child.contains("PGP"), "{new_child}");
        assert!(new_child.ends_with("\n\nsigned work\n"));
        assert!(new_child.contains("committer A <a@x> 1700000000 +0000"), "committer kept");
        let new_tag = run(&dir, &["cat-file", "tag", "v1"]).unwrap();
        assert!(new_tag.contains("Release one") && !new_tag.contains("PGP"), "{new_tag}");
        assert_eq!(run(&dir, &["rev-parse", "v1^{commit}"]).unwrap(), run(&dir, &["rev-parse", "main"]).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reword_waits_for_an_unfinished_merge() {
        let dir = temp_repo("reword-conflict");
        commit_file_with(&dir, "a.txt", "1\n", "one");
        run(&dir, &["checkout", "-q", "-b", "x"]).unwrap();
        commit_file_with(&dir, "a.txt", "x\n", "x side");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        commit_file_with(&dir, "a.txt", "main\n", "main side");
        assert!(run(&dir, &["merge", "x"]).is_err(), "conflict");
        let before = run(&dir, &["rev-parse", "main"]).unwrap();
        let e = reword(&dir, &commit_id(&dir, "one"), "uno").unwrap_err();
        assert!(e.contains("in progress"), "{e}");
        assert_eq!(run(&dir, &["rev-parse", "main"]).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Merges, rebases, cherry-picks and reverts in progress

    #[test]
    fn conflict_markers_in_every_style() {
        let merge = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> x\nb\n";
        let segs = parse_conflicts(merge, 7);
        assert_eq!(segs.len(), 3);
        let Segment::Conflict(r) = &segs[1] else { panic!("{segs:?}") };
        assert_eq!((r.ours.clone(), r.theirs.clone(), r.base.clone()), (vec!["ours\n".to_string()], vec!["theirs\n".to_string()], None));
        assert_eq!(segs[0], Segment::Same(vec!["a\n".into()]));
        assert_eq!(r.original(), "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> x\n");

        // diff3 and zdiff3 both carry the base after |||||||.
        let diff3 = "<<<<<<< ours\no\n||||||| base\nb1\nb2\n=======\nt\n>>>>>>> theirs\n";
        let Segment::Conflict(r) = &parse_conflicts(diff3, 7)[0] else { panic!() };
        assert_eq!(r.base, Some(vec!["b1\n".to_string(), "b2\n".to_string()]));
        assert_eq!(r.original(), diff3);
        // An empty side is fine, and Windows line endings stay on each line.
        let crlf = "x\r\n<<<<<<< HEAD\r\n=======\r\nnew\r\n>>>>>>> y\r\n";
        let segs = parse_conflicts(crlf, 7);
        let Segment::Conflict(r) = &segs[1] else { panic!() };
        assert!(r.ours.is_empty() && r.theirs == vec!["new\r\n".to_string()]);
        assert!(uses_crlf(crlf));
        // A bigger marker size; 7-character lines inside are ordinary text.
        let nine = "<<<<<<<<< a\n=======\n=========\nt\n>>>>>>>>> b";
        let Segment::Conflict(r) = &parse_conflicts(nine, 9)[0] else { panic!() };
        assert_eq!(r.ours, vec!["=======\n".to_string()]);
        assert_eq!(r.markers[3], ">>>>>>>>> b", "no newline at the very end");
        // A marker needs a space or nothing after it; an unfinished region is plain text.
        let odd = "<<<<<<<x\n<<<<<<< a\nfoo\n";
        assert_eq!(parse_conflicts(odd, 7), vec![Segment::Same(vec!["<<<<<<<x\n".into(), "<<<<<<< a\n".into(), "foo\n".into()])]);
        assert!(parse_conflicts("", 7).is_empty());
    }

    /// main and `feature` change lines 2, 10 and 18 of a 20-line file differently.
    fn diverged(name: &str) -> PathBuf {
        let dir = temp_repo(name);
        let base: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        commit_file_with(&dir, "a.txt", &base.concat(), "Base");
        run(&dir, &["checkout", "-q", "-b", "feature"]).unwrap();
        let mut f = base.clone();
        for i in [1, 9, 17] {
            f[i] = format!("feature {}\n", i + 1);
        }
        commit_file_with(&dir, "a.txt", &f.concat(), "Feature edits");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        let mut m = base;
        for i in [1, 9, 17] {
            m[i] = format!("main {}\n", i + 1);
        }
        commit_file_with(&dir, "a.txt", &m.concat(), "Main edits");
        dir
    }

    #[test]
    fn merge_conflict_names_both_sides_and_continues() {
        let dir = diverged("op-merge");
        assert_eq!(operation(&dir).unwrap(), None);
        assert!(run(&dir, &["merge", "feature"]).is_err());
        let op = operation(&dir).unwrap().unwrap();
        assert_eq!((op.kind, op.branch.as_deref(), op.ours.as_str(), op.theirs.as_str()), (OpType::Merge, Some("main"), "main", "feature"));
        assert_eq!(op.title(), "Merging feature into main");
        assert!(!op.can_skip() && op.merge_tool.is_none());
        let st = status(&dir).unwrap();
        assert_eq!(st.conflicts[0].change, Change::Conflict(ConflictKind::BothModified));

        // Default markers have no base, so it's rebuilt from the index for each region.
        let f = conflict_file(&dir, "a.txt", ConflictKind::BothModified).unwrap();
        assert!(!f.binary && f.index_base);
        let regions: Vec<&Region> = f.regions().collect();
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[1].ours, vec!["main 10\n".to_string()]);
        assert_eq!(regions[1].theirs, vec!["feature 10\n".to_string()]);
        assert_eq!(regions[1].base, Some(vec!["line 10\n".to_string()]));
        // Continue is refused while the file is unresolved.
        assert!(continue_operation(&dir, OpType::Merge, None).is_err());
        save_resolution(&dir, "a.txt", "resolved\n", true).unwrap();
        continue_operation(&dir, OpType::Merge, Some("Merge feature, by hand")).unwrap();
        assert_eq!(operation(&dir).unwrap(), None);
        assert_eq!(message_of(&dir, "HEAD"), "Merge feature, by hand");
        assert_eq!(run(&dir, &["rev-list", "--parents", "-1", "HEAD"]).unwrap().split(' ').count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zdiff3_markers_carry_their_own_base_and_merge_continues_with_its_message() {
        let dir = diverged("op-zdiff3");
        run(&dir, &["config", "merge.conflictStyle", "zdiff3"]).unwrap();
        assert!(run(&dir, &["merge", "feature"]).is_err());
        let f = conflict_file(&dir, "a.txt", ConflictKind::BothModified).unwrap();
        assert!(!f.index_base);
        assert!(f.regions().all(|r| r.base.as_ref().is_some_and(|b| b.len() == 1)));
        // Whole-file choices, then mark resolved and continue with git's own message.
        take_side(&dir, "a.txt", Side::Theirs).unwrap();
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("feature 10"));
        take_side(&dir, "a.txt", Side::Ours).unwrap();
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("main 10"));
        mark_resolved(&dir, "a.txt").unwrap();
        continue_operation(&dir, OpType::Merge, None).unwrap();
        assert!(message_of(&dir, "HEAD").starts_with("Merge branch 'feature'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebase_conflicts_count_steps_skip_and_abort() {
        for (name, backend) in [("op-rebase", "--merge"), ("op-rebase-apply", "--apply")] {
            let dir = diverged(name);
            run(&dir, &["checkout", "-q", "feature"]).unwrap();
            commit_file_with(&dir, "b.txt", "more\n", "Second feature commit");
            let before = run(&dir, &["rev-parse", "feature"]).unwrap();
            assert!(run(&dir, &["rebase", backend, "main"]).is_err());
            let op = operation(&dir).unwrap().unwrap();
            assert_eq!(op.kind, OpType::Rebase, "{backend}");
            assert_eq!((op.branch.as_deref(), op.ours.as_str()), (Some("feature"), "main"), "{backend}");
            assert_eq!(op.step, Some((1, 2)), "{backend}");
            assert!(op.theirs.ends_with("“Feature edits”"), "{backend}: {}", op.theirs);
            assert!(op.title().starts_with("Rebasing feature onto main · commit 1 of 2"), "{}", op.title());
            assert!(op.can_skip());
            assert_eq!(op.before.as_deref(), Some(&short_id(&dir, before.trim())[..]));
            abort_operation(&dir, OpType::Rebase).unwrap();
            assert_eq!(operation(&dir).unwrap(), None);
            assert_eq!(run(&dir, &["rev-parse", "feature"]).unwrap(), before);
            // Skipping the conflicting commit replays the rest.
            assert!(run(&dir, &["rebase", backend, "main"]).is_err());
            skip_operation(&dir, OpType::Rebase).unwrap();
            assert_eq!(operation(&dir).unwrap(), None);
            assert_eq!(message_of(&dir, "HEAD"), "Second feature commit");
            assert_eq!(message_of(&dir, "HEAD~1"), "Main edits");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn cherry_pick_alone_and_in_a_sequence() {
        let dir = diverged("op-pick");
        run(&dir, &["checkout", "-q", "feature"]).unwrap();
        commit_file_with(&dir, "a.txt", "feature rewrite\n", "Rewrite on feature");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        assert!(run(&dir, &["cherry-pick", "feature~1"]).is_err());
        let op = operation(&dir).unwrap().unwrap();
        assert_eq!((op.kind, op.ours.as_str(), op.remaining), (OpType::CherryPick, "main", 0));
        assert!(op.theirs.ends_with("“Feature edits”") && op.title().starts_with("Cherry-picking "), "{}", op.title());
        assert!(!op.can_skip(), "a single pick has nothing to skip to");
        abort_operation(&dir, OpType::CherryPick).unwrap();

        // Two picks: the first stops, one more waits; skip it and the second stops too.
        assert!(run(&dir, &["cherry-pick", "feature~1", "feature"]).is_err());
        let op = operation(&dir).unwrap().unwrap();
        assert_eq!(op.remaining, 1);
        assert!(op.can_skip() && op.title().ends_with("· 1 more after this"), "{}", op.title());
        assert!(skip_operation(&dir, OpType::CherryPick).is_err(), "the next pick conflicts as well");
        let op = operation(&dir).unwrap().unwrap();
        assert!(op.theirs.ends_with("“Rewrite on feature”"), "{}", op.theirs);
        save_resolution(&dir, "a.txt", "picked\n", true).unwrap();
        continue_operation(&dir, OpType::CherryPick, None).unwrap();
        assert_eq!(operation(&dir).unwrap(), None);
        assert_eq!(message_of(&dir, "HEAD"), "Rewrite on feature");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revert_conflict_is_detected_and_continues() {
        let dir = diverged("op-revert");
        commit_file_with(&dir, "a.txt", "rewritten\n", "Rewrite");
        assert!(run(&dir, &["revert", "--no-edit", "HEAD~1"]).is_err());
        let op = operation(&dir).unwrap().unwrap();
        assert_eq!(op.kind, OpType::Revert);
        assert!(op.title().starts_with("Reverting ") && op.title().contains("“Main edits”"), "{}", op.title());
        assert!(op.theirs.starts_with("before "));
        save_resolution(&dir, "a.txt", "reverted\n", true).unwrap();
        continue_operation(&dir, OpType::Revert, None).unwrap();
        assert_eq!(operation(&dir).unwrap(), None);
        assert!(message_of(&dir, "HEAD").starts_with("Revert \"Main edits\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_modify_add_add_and_binary_conflicts() {
        let dir = temp_repo("op-kinds");
        commit_file_with(&dir, "gone-here.txt", "1\n", "Base 1");
        commit_file_with(&dir, "gone-there.txt", "1\n", "Base 2");
        std::fs::write(dir.join("pic.bin"), [0u8, 1, 2]).unwrap();
        commit_file_with(&dir, "pic.bin", "\0base", "Base 3");
        run(&dir, &["checkout", "-q", "-b", "other"]).unwrap();
        commit_file_with(&dir, "gone-here.txt", "changed there\n", "Change");
        run(&dir, &["rm", "-q", "gone-there.txt"]).unwrap();
        commit_file_with(&dir, "both.txt", "theirs\n", "Add both + delete");
        commit_file_with(&dir, "pic.bin", "\0theirs", "Bin theirs");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        run(&dir, &["rm", "-q", "gone-here.txt"]).unwrap();
        commit_file_with(&dir, "gone-there.txt", "changed here\n", "Delete + change");
        commit_file_with(&dir, "both.txt", "ours\n", "Add both");
        commit_file_with(&dir, "pic.bin", "\0ours!", "Bin ours");
        assert!(run(&dir, &["merge", "other"]).is_err());

        let kinds: HashMap<String, Change> = status(&dir).unwrap().conflicts.into_iter().map(|c| (c.path, c.change)).collect();
        assert_eq!(kinds["gone-here.txt"], Change::Conflict(ConflictKind::DeletedByUs));
        assert_eq!(kinds["gone-there.txt"], Change::Conflict(ConflictKind::DeletedByThem));
        assert_eq!(kinds["both.txt"], Change::Conflict(ConflictKind::BothAdded));
        assert_eq!(kinds["pic.bin"], Change::Conflict(ConflictKind::BothModified));
        assert!(ConflictKind::BothAdded.has_regions() && !ConflictKind::DeletedByUs.has_regions());

        // Add/add has markers with an empty base.
        let f = conflict_file(&dir, "both.txt", ConflictKind::BothAdded).unwrap();
        assert_eq!(f.regions().count(), 1);
        assert_eq!(f.regions().next().unwrap().base, Some(vec![]));
        let bin = conflict_file(&dir, "pic.bin", ConflictKind::BothModified).unwrap();
        assert!(bin.binary && bin.segments.is_empty());
        assert_eq!(bin.sizes, [Some(5), Some(6), Some(7)]);
        let gone = conflict_file(&dir, "gone-there.txt", ConflictKind::DeletedByThem).unwrap();
        assert_eq!(gone.sizes, [Some(2), Some(13), None]);
        // What each side did: ours changed "1" to "changed here", theirs deleted "1".
        let sided = gone.sided.unwrap();
        assert_eq!((sided.stage, sided.base.clone()), (2, vec!["1".to_string()]));
        let changes: Vec<(LineKind, &str)> = sided.changes.iter().map(|l| (l.kind.clone(), l.text.as_str())).collect();
        assert_eq!(changes, [(LineKind::Del, "1"), (LineKind::Add, "changed here")]);
        let here = conflict_file(&dir, "gone-here.txt", ConflictKind::DeletedByUs).unwrap().sided.unwrap();
        assert_eq!(here.stage, 3, "only theirs still has it");

        // Keep takes the side that still has the file; delete removes it everywhere.
        // Every file's choice is applied in one go.
        resolve_files(
            &dir,
            &[
                Resolution::Keep("gone-here.txt".into()),
                Resolution::Delete("gone-there.txt".into()),
                Resolution::Delete("both.txt".into()),
                Resolution::Side("pic.bin".into(), Side::Theirs),
            ],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("gone-here.txt")).unwrap(), "changed there\n");
        assert!(!dir.join("gone-there.txt").exists());
        assert!(status(&dir).unwrap().conflicts.is_empty());
        assert_eq!(std::fs::read(dir.join("pic.bin")).unwrap(), b"\0theirs");
        continue_operation(&dir, OpType::Merge, None).unwrap();
        assert_eq!(run(&dir, &["ls-files"]).unwrap(), "gone-here.txt\npic.bin\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn operations_in_a_linked_worktree_stay_in_that_worktree() {
        let dir = diverged("op-worktree");
        let wt = std::env::temp_dir().join(format!("headway-test-op-worktree-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        run(&dir, &["branch", "side", "main"]).unwrap();
        add_worktree(&dir, &wt, "side", None).unwrap();
        assert!(run(&wt, &["merge", "feature"]).is_err());
        let op = operation(&wt).unwrap().unwrap();
        assert_eq!((op.kind, op.ours.as_str(), op.theirs.as_str()), (OpType::Merge, "side", "feature"));
        assert_eq!(operation(&dir).unwrap(), None, "the main worktree isn't merging");
        assert_eq!(conflict_file(&wt, "a.txt", ConflictKind::BothModified).unwrap().regions().count(), 3);
        abort_operation(&wt, OpType::Merge).unwrap();
        assert_eq!(operation(&wt).unwrap(), None);
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_merge_tool_and_custom_marker_size() {
        let dir = diverged("op-tool");
        std::fs::write(dir.join(".gitattributes"), "a.txt conflict-marker-size=10\n").unwrap();
        assert!(run(&dir, &["merge", "feature"]).is_err());
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("<<<<<<<<<< "));
        assert_eq!(marker_size(&dir, "a.txt"), 10);
        let f = conflict_file(&dir, "a.txt", ConflictKind::BothModified).unwrap();
        assert_eq!(f.regions().count(), 3);
        assert!(f.index_base);

        // A "tool" that just takes their side.
        run(&dir, &["config", "merge.tool", "takeTheirs"]).unwrap();
        run(&dir, &["config", "mergetool.takeTheirs.cmd", "cp \"$REMOTE\" \"$MERGED\""]).unwrap();
        run(&dir, &["config", "mergetool.takeTheirs.trustExitCode", "true"]).unwrap();
        run(&dir, &["config", "mergetool.keepBackup", "false"]).unwrap();
        assert_eq!(operation(&dir).unwrap().unwrap().merge_tool.as_deref(), Some("takeTheirs"));
        run_merge_tool(&dir, "a.txt").unwrap();
        assert!(status(&dir).unwrap().conflicts.is_empty(), "the tool's result is staged");
        assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("feature 10"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_no_branch_points_at_is_named_by_its_id() {
        let dir = diverged("op-unnamed");
        run(&dir, &["checkout", "-q", "feature"]).unwrap();
        commit_file_with(&dir, "b.txt", "later\n", "Later");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        assert!(run(&dir, &["merge", "feature~1"]).is_err());
        let op = operation(&dir).unwrap().unwrap();
        assert_eq!(op.theirs, short_id(&dir, "feature~1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolving_carries_on_past_a_failure_and_reports_it() {
        let dir = diverged("op-resolve-fail");
        assert!(run(&dir, &["merge", "feature"]).is_err());
        let e = resolve_files(&dir, &[Resolution::Keep("not-conflicted.txt".into()), Resolution::Text("a.txt".into(), "done\n".into())]).unwrap_err();
        assert!(e.contains("no version to keep"), "{e}");
        assert!(status(&dir).unwrap().conflicts.is_empty(), "a.txt was still resolved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Put versions of `path` straight into the index as conflict stages.
    fn stage_versions(dir: &Path, path: &str, versions: &[(u8, &[u8])]) {
        let mut info = String::new();
        for (stage, bytes) in versions {
            let tmp = dir.join(format!(".blob{stage}"));
            std::fs::write(&tmp, bytes).unwrap();
            let oid = run(dir, &["hash-object", "-w", tmp.to_str().unwrap()]).unwrap();
            std::fs::remove_file(tmp).unwrap();
            info += &format!("100644 {} {stage}\t{path}\n", oid.trim());
        }
        run(dir, &["update-index", "--force-remove", "--", path]).unwrap();
        run_stdin(dir, &["update-index", "--index-info"], &info).unwrap();
    }

    #[test]
    fn one_sided_files_without_an_ancestor_and_files_both_sides_deleted() {
        let dir = temp_repo("op-sided");
        commit_file_with(&dir, "seed.txt", "x\n", "Seed");
        // Only theirs has it, with no common ancestor: every line is new.
        stage_versions(&dir, "new.txt", &[(3, b"one\ntwo\n")]);
        // Both sides deleted it: only the ancestor is left.
        stage_versions(&dir, "old.txt", &[(1, b"gone\n")]);
        // Ours kept it unchanged while theirs deleted it.
        stage_versions(&dir, "same.txt", &[(1, b"keep\n"), (2, b"keep\n")]);
        // A binary file that isn't in the folder: judged by the index.
        stage_versions(&dir, "pic.bin", &[(2, b"\0a"), (3, b"\0b")]);
        let kinds: HashMap<String, Change> = status(&dir).unwrap().conflicts.into_iter().map(|c| (c.path, c.change)).collect();
        assert_eq!(kinds["new.txt"], Change::Conflict(ConflictKind::AddedByThem));
        assert_eq!(kinds["old.txt"], Change::Conflict(ConflictKind::BothDeleted));
        assert_eq!(kinds["same.txt"], Change::Conflict(ConflictKind::DeletedByThem));

        let new = conflict_file(&dir, "new.txt", ConflictKind::AddedByThem).unwrap().sided.unwrap();
        assert_eq!(new.stage, 3);
        assert!(new.base.is_empty());
        let lines: Vec<(LineKind, Option<u32>, &str)> = new.changes.iter().map(|l| (l.kind.clone(), l.new_no, l.text.as_str())).collect();
        assert_eq!(lines, [(LineKind::Add, Some(1), "one"), (LineKind::Add, Some(2), "two")]);
        let old = conflict_file(&dir, "old.txt", ConflictKind::BothDeleted).unwrap().sided.unwrap();
        assert_eq!((old.stage, old.base.clone(), old.changes.len()), (1, vec!["gone".to_string()], 0));
        let same = conflict_file(&dir, "same.txt", ConflictKind::DeletedByThem).unwrap().sided.unwrap();
        assert_eq!(same.changes.iter().map(|l| (l.kind.clone(), l.text.as_str())).collect::<Vec<_>>(), [(LineKind::Context, "keep")]);
        assert!(conflict_file(&dir, "pic.bin", ConflictKind::BothModified).unwrap().binary);

        // Keeping a both-deleted file brings the ancestor's version back.
        keep_file(&dir, "old.txt").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("old.txt")).unwrap(), "gone\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewriting_raw_commits() {
        let raw = "tree t\nparent p1\nparent p2\nauthor A <a> 1 +0000\ncommitter C <c> 2 +0000\nencoding ISO-8859-1\ngpgsig -----BEGIN PGP SIGNATURE-----\n line\n -----END PGP SIGNATURE-----\nmergetag object m\n type commit\n\nSubject\n\nBody\n";
        let new = rewrite_commit(raw, Some(&["n1".to_string(), "n2".to_string()]), None);
        assert_eq!(
            new,
            "tree t\nparent n1\nparent n2\nauthor A <a> 1 +0000\ncommitter C <c> 2 +0000\nencoding ISO-8859-1\nmergetag object m\n type commit\n\nSubject\n\nBody\n"
        );
        let msg = rewrite_commit(raw, None, Some("New subject\n\n\n"));
        assert!(msg.contains("parent p1\nparent p2\n") && msg.ends_with("\n\nNew subject\n"));
        assert_eq!(rewrite_tag("object a\ntype tag\ntag t\n\nx\n", "b"), None, "tags of tags are left alone");
    }

    #[test]
    fn parse_worktree_list() {
        let out = "worktree /code/app
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /code/app-feature
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feature/login
locked

worktree /code/app-old
HEAD 3333333333333333333333333333333333333333
detached
prunable gitdir file points to non-existent location
";
        let w = parse_worktrees(out, Path::new("/code/app-feature"));
        assert_eq!(w.len(), 3);
        assert!(w[0].is_main && !w[1].is_main);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert_eq!(w[1].branch.as_deref(), Some("feature/login"));
        assert!(w[1].locked && w[1].is_current && !w[0].is_current);
        assert!(w[2].detached && w[2].prunable && w[2].branch.is_none());
        assert_eq!(w[2].head, "3333333333333333333333333333333333333333");
        assert!(parse_worktrees("", Path::new("/x")).is_empty());
    }

    #[test]
    fn worktree_folder_names() {
        assert_eq!(default_worktree_path(Path::new("/code/app"), "feature/login"), PathBuf::from("/code/app-feature-login"));
        assert_eq!(default_worktree_path(Path::new("/code/app"), " fix bug "), PathBuf::from("/code/app-fix-bug"));
    }

    #[test]
    fn create_list_and_remove_worktrees() {
        let dir = temp_repo("wt");
        std::fs::write(dir.join("f.txt"), "1\n").unwrap();
        stage(&dir, &["f.txt".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        run(&dir, &["branch", "existing"]).unwrap();

        let new_wt = dir.with_file_name(format!("{}-new", repo_name(&dir)));
        let old_wt = dir.with_file_name(format!("{}-existing", repo_name(&dir)));
        let _ = std::fs::remove_dir_all(&new_wt);
        let _ = std::fs::remove_dir_all(&old_wt);
        add_worktree(&dir, &new_wt, "topic", Some("HEAD")).unwrap();
        add_worktree(&dir, &old_wt, "existing", None).unwrap();
        // A branch can only be checked out in one place.
        assert!(add_worktree(&dir, &dir.with_file_name("headway-wt-dupe"), "existing", None).is_err());

        let list = worktrees(&dir).unwrap();
        assert_eq!(list.len(), 3);
        assert!(list[0].is_main && list[0].is_current);
        let topic = list.iter().find(|w| w.branch.as_deref() == Some("topic")).unwrap();
        assert!(same_path(&topic.path, &new_wt));
        // Seen from inside the new worktree, that one is current and it's a repository too.
        let from_inside = worktrees(&new_wt).unwrap();
        assert!(from_inside.iter().any(|w| w.is_current && w.branch.as_deref() == Some("topic")));
        assert!(is_repo(&new_wt));
        assert_eq!(status(&new_wt).unwrap().head.branch.as_deref(), Some("topic"));

        // Removing a worktree with changes needs force.
        std::fs::write(new_wt.join("f.txt"), "changed\n").unwrap();
        assert!(remove_worktree(&dir, &new_wt, false).is_err());
        remove_worktree(&dir, &new_wt, true).unwrap();
        remove_worktree(&dir, &old_wt, false).unwrap();
        assert_eq!(worktrees(&dir).unwrap().len(), 1);
        assert!(!new_wt.exists());

        // A worktree whose folder vanished can be cleaned up.
        add_worktree(&dir, &new_wt, "topic", None).unwrap();
        std::fs::remove_dir_all(&new_wt).unwrap();
        assert!(worktrees(&dir).unwrap().iter().any(|w| w.prunable));
        run(&dir, &["worktree", "prune"]).unwrap();
        assert_eq!(worktrees(&dir).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Reading diffs (no git needed)

    const SAMPLE_PATCH: &str = "diff --git a/src/app.js b/src/app.js
index 1111111..2222222 100644
--- a/src/app.js
+++ b/src/app.js
@@ -1,4 +1,5 @@ function main() {
 one
-two
+TWO
+two and a half
 three
\\ No newline at end of file
@@ -10,2 +11,2 @@
 ten
-eleven
+ELEVEN
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..3333333
Binary files /dev/null and b/img.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 4444444..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-bye
-now
";

    #[test]
    fn parse_patch_reads_files_hunks_and_line_numbers() {
        let files = parse_patch(SAMPLE_PATCH);
        assert_eq!(files.len(), 4);

        let f = &files[0];
        assert_eq!(f.path, "src/app.js");
        assert_eq!(f.change, Change::Modified);
        assert_eq!((f.added, f.removed), (3, 2));
        assert_eq!(f.hunks.len(), 2);
        let h = &f.hunks[0];
        assert!(h.header.starts_with("@@ -1,4 +1,5 @@"));
        let kinds: Vec<&LineKind> = h.lines.iter().map(|l| &l.kind).collect();
        assert_eq!(
            kinds,
            [&LineKind::Context, &LineKind::Del, &LineKind::Add, &LineKind::Add, &LineKind::Context, &LineKind::NoNewline]
        );
        // Old and new line numbers advance independently.
        assert_eq!((h.lines[0].old_no, h.lines[0].new_no), (Some(1), Some(1)));
        assert_eq!((h.lines[1].old_no, h.lines[1].new_no), (Some(2), None));
        assert_eq!((h.lines[3].old_no, h.lines[3].new_no), (None, Some(3)));
        assert_eq!((h.lines[4].old_no, h.lines[4].new_no), (Some(3), Some(4)));
        assert_eq!(f.hunks[1].lines[2].new_no, Some(12));

        let r = &files[1];
        assert_eq!(r.change, Change::Renamed);
        assert_eq!(r.path, "new name.txt");
        assert_eq!(r.old_path.as_deref(), Some("old name.txt"));
        assert!(r.hunks.is_empty());

        assert!(files[2].binary);
        assert_eq!(files[2].change, Change::Added);

        assert_eq!(files[3].change, Change::Deleted);
        assert_eq!(files[3].removed, 2);
    }

    #[test]
    fn hunk_patch_rebuilds_one_hunk() {
        let f = &parse_patch(SAMPLE_PATCH)[0];
        let p = f.hunk_patch(1).unwrap();
        assert!(p.starts_with("diff --git a/src/app.js b/src/app.js\n"));
        assert!(p.contains("@@ -10,2 +11,2 @@\n ten\n-eleven\n+ELEVEN\n"));
        assert!(!p.contains("TWO"));
        assert!(f.hunk_patch(5).is_none());
    }

    #[test]
    fn decorations_become_labels() {
        let refs = parse_decorations(
            "HEAD -> refs/heads/main, refs/remotes/origin/main, refs/remotes/origin/HEAD, tag: refs/tags/v1.0, refs/heads/feature/x, refs/notes/commits",
        );
        let got: Vec<(&str, RefKind)> = refs.iter().map(|r| (r.name.as_str(), r.kind.clone())).collect();
        assert_eq!(
            got,
            [
                ("main", RefKind::Head),
                ("origin/main", RefKind::Remote),
                ("v1.0", RefKind::Tag),
                // A local branch with a slash stays local.
                ("feature/x", RefKind::Local),
            ]
        );
        assert!(parse_decorations("").is_empty());
        assert_eq!(parse_decorations("HEAD")[0].kind, RefKind::Head);
    }

    #[test]
    fn change_count_counts_each_path_once() {
        let fc = |p: &str, c: Change| FileChange { path: p.into(), old_path: None, change: c, submodule: false };
        let mut st = Status {
            staged: vec![fc("a", Change::Modified)],
            unstaged: vec![fc("a", Change::Modified), fc("b", Change::Untracked)],
            conflicts: vec![fc("c", Change::Conflict(ConflictKind::BothModified))],
            ..Default::default()
        };
        st.changes = st.count_changes();
        assert_eq!(st.change_count(), 3);
    }

    // -----------------------------------------------------------------------
    // Building patches from picked lines (no git needed)

    fn one_hunk() -> DiffFile {
        parse_patch(
            "diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,4 +1,4 @@
 keep
-old 1
-old 2
+new 1
+new 2
 end
",
        )
        .remove(0)
    }

    #[test]
    fn partial_patch_forward_keeps_unpicked_removals_as_context() {
        let f = one_hunk();
        // Pick "-old 1" (line 1) and "+new 2" (line 4).
        let sel: LineSelection = [(0, 1), (0, 4)].into_iter().collect();
        let p = f.partial_patch(&sel, false).unwrap();
        assert!(p.ends_with("@@ -1,4 +1,4 @@\n keep\n-old 1\n old 2\n+new 2\n end\n"), "{p}");
    }

    #[test]
    fn partial_patch_reverse_keeps_unpicked_additions_as_context() {
        let f = one_hunk();
        let sel: LineSelection = [(0, 1), (0, 4)].into_iter().collect();
        let p = f.partial_patch(&sel, true).unwrap();
        assert!(p.ends_with("@@ -1,4 +1,4 @@\n keep\n-old 1\n new 1\n+new 2\n end\n"), "{p}");
    }

    #[test]
    fn partial_patch_skips_unpicked_hunks_and_needs_a_pick() {
        let f = &parse_patch(SAMPLE_PATCH)[0];
        assert!(f.partial_patch(&LineSelection::new(), false).is_none());
        // Picking a context line alone picks nothing.
        assert!(f.partial_patch(&[(0, 0)].into_iter().collect(), false).is_none());
        // Only the second hunk's "+ELEVEN".
        let p = f.partial_patch(&[(1, 2)].into_iter().collect(), false).unwrap();
        assert!(!p.contains("TWO"));
        assert!(p.contains("@@ -10,2 +10,3 @@\n ten\n eleven\n+ELEVEN\n"), "{p}");
    }

    #[test]
    fn partial_patch_turns_new_and_deleted_headers_into_modifications() {
        let deleted = &parse_patch(SAMPLE_PATCH)[3];
        // Everything picked: the real deletion header stays.
        let all = deleted.partial_patch(&[(0, 0), (0, 1)].into_iter().collect(), false).unwrap();
        assert!(all.contains("deleted file mode") && all.contains("+++ /dev/null"));
        // Some picked: it must become an ordinary change to the file.
        let some = deleted.partial_patch(&[(0, 0)].into_iter().collect(), false).unwrap();
        assert!(!some.contains("deleted file mode") && !some.contains("/dev/null"));
        assert!(some.contains("+++ b/gone.txt"));
        assert!(some.ends_with("@@ -1,2 +1,1 @@\n-bye\n now\n"), "{some}");
    }

    // -----------------------------------------------------------------------
    // Against real repositories

    fn commit_file(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
        stage(dir, &[name.into()]).unwrap();
        commit(dir, &format!("write {name}"), false).unwrap();
    }

    #[test]
    fn stage_lines_from_a_later_hunk_only() {
        let dir = temp_repo("laterhunk");
        let original: String = (1..=40).map(|i| format!("l{i}\n")).collect();
        commit_file(&dir, "f.txt", &original);
        let edited = original.replace("l2\n", "l2\nadded top\n").replace("l35\n", "l35\nadded bottom\n");
        std::fs::write(dir.join("f.txt"), &edited).unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert_eq!(d.hunks.len(), 2);
        let sel = changed_lines(&d, |l| l.text == "added bottom");
        stage_lines(&dir, &d, &sel, false).unwrap();
        let idx = index_text(&dir, "f.txt");
        assert!(idx.contains("l35\nadded bottom\nl36") && !idx.contains("added top"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unstage_lines_across_two_hunks() {
        let dir = temp_repo("twohunks");
        let original: String = (1..=40).map(|i| format!("l{i}\n")).collect();
        commit_file(&dir, "f.txt", &original);
        let edited = original.replace("l5\n", "L5\n").replace("l30\n", "L30\n");
        std::fs::write(dir.join("f.txt"), &edited).unwrap();
        stage(&dir, &["f.txt".into()]).unwrap();
        let staged = file_diff(&dir, "f.txt", true, false).unwrap().unwrap();
        assert_eq!(staged.hunks.len(), 2);
        // Unstage both changes of the second hunk only.
        let sel: LineSelection = changed_lines(&staged, |_| true).into_iter().filter(|(h, _)| *h == 1).collect();
        unstage_lines(&dir, &staged, &sel).unwrap();
        let idx = index_text(&dir, "f.txt");
        assert!(idx.contains("L5\n") && idx.contains("l30\n") && !idx.contains("L30"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_part_of_a_deleted_file() {
        let dir = temp_repo("delpart");
        commit_file(&dir, "d.txt", "a\nb\nc\n");
        std::fs::remove_file(dir.join("d.txt")).unwrap();
        let d = file_diff(&dir, "d.txt", false, false).unwrap().unwrap();
        let sel = changed_lines(&d, |l| l.text == "b");
        stage_lines(&dir, &d, &sel, false).unwrap();
        assert_eq!(index_text(&dir, "d.txt"), "a\nc\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_lines_with_nothing_picked_is_an_error() {
        let dir = temp_repo("nopick");
        commit_file(&dir, "f.txt", "a\n");
        std::fs::write(dir.join("f.txt"), "b\n").unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert!(stage_lines(&dir, &d, &LineSelection::new(), false).is_err());
        assert!(file_diff(&dir, "f.txt", true, false).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn personal_git_settings_do_not_change_what_we_read() {
        let dir = temp_repo("settings");
        for (k, v) in [
            ("diff.mnemonicPrefix", "true"),
            ("diff.noprefix", "true"),
            ("color.ui", "always"),
            ("color.diff", "always"),
            ("log.showSignature", "true"),
        ] {
            run(&dir, &["config", k, v]).unwrap();
        }
        commit_file(&dir, "f.txt", "one\ntwo\n");
        std::fs::write(dir.join("f.txt"), "one\nTWO\n").unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        assert_eq!(d.path, "f.txt");
        assert!(d.header.contains("--- a/f.txt") && d.header.contains("+++ b/f.txt"));
        assert!(!d.header.contains('\x1b'));
        let sel = changed_lines(&d, |_| true);
        stage_lines(&dir, &d, &sel, false).unwrap();
        assert_eq!(index_text(&dir, "f.txt"), "one\nTWO\n");
        let commits = log(&dir, 10).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].summary, "write f.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_reports_ahead_and_behind() {
        let up = temp_repo("upstream");
        commit_file(&up, "f.txt", "1\n");
        let down = std::env::temp_dir().join(format!("headway-test-downstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&down);
        run(&std::env::temp_dir(), &["clone", "-q", up.to_str().unwrap(), down.to_str().unwrap()]).unwrap();
        run(&down, &["config", "user.name", "Test"]).unwrap();
        run(&down, &["config", "user.email", "t@example.com"]).unwrap();
        commit_file(&up, "g.txt", "2\n");
        commit_file(&down, "h.txt", "3\n");
        run(&down, &["fetch", "-q"]).unwrap();
        let st = status(&down).unwrap();
        assert_eq!(st.head.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.head.ahead, st.head.behind), (1, 1));
        let (branches, _) = branches(&down).unwrap();
        assert!(branches.iter().any(|b| b.remote && b.name == "origin/main"));
        assert_eq!(remotes(&down).unwrap(), ["origin"]);
        let _ = std::fs::remove_dir_all(&up);
        let _ = std::fs::remove_dir_all(&down);
    }

    #[test]
    fn stashes_tags_and_scanning_for_repos() {
        let dir = temp_repo("misc");
        commit_file(&dir, "f.txt", "1\n");
        run(&dir, &["tag", "v1"]).unwrap();
        std::fs::write(dir.join("f.txt"), "2\n").unwrap();
        run(&dir, &["stash", "push", "-m", "half done"]).unwrap();
        let s = stashes(&dir).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].message.contains("half done"));
        let (_, tags) = branches(&dir).unwrap();
        assert_eq!(tags, ["v1"]);
        assert!(log(&dir, 5).unwrap()[0].refs.iter().any(|r| r.kind == RefKind::Tag && r.name == "v1"));
        run(&dir, &["branch", "fix/typo"]).unwrap();
        let refs = &log(&dir, 5).unwrap()[0].refs;
        assert!(refs.iter().any(|r| r.kind == RefKind::Local && r.name == "fix/typo"), "{refs:?}");
        assert!(refs.iter().any(|r| r.kind == RefKind::Head && r.name == "main"));

        // A parent folder holding two repositories and a plain folder.
        let parent = std::env::temp_dir().join(format!("headway-test-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        for name in ["b-repo", "a-repo", "plain"] {
            std::fs::create_dir_all(parent.join(name)).unwrap();
        }
        run(&parent.join("a-repo"), &["init", "-q"]).unwrap();
        run(&parent.join("b-repo"), &["init", "-q"]).unwrap();
        let found = scan_for_repos(&parent);
        assert_eq!(found, [parent.join("a-repo"), parent.join("b-repo")]);
        assert_eq!(scan_for_repos(&parent.join("a-repo")), [parent.join("a-repo")]);
        assert_eq!(repo_name(&parent.join("a-repo")), "a-repo");
        let _ = std::fs::remove_dir_all(&parent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Undo

    /// Run `op` the way the app runs an undoable action, and return its log entry.
    fn recorded(dir: &Path, what: Undoable, op: impl FnOnce()) -> UndoEntry {
        let prep = prepare_undo(dir, &what).unwrap();
        op();
        finish_undo(dir, prep, "test action").unwrap().expect("the action changed something")
    }

    fn head(dir: &Path) -> String {
        run(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    #[test]
    fn undo_a_commit_keeps_its_changes_staged() {
        let dir = temp_repo("undo-commit");
        commit_file(&dir, "a.txt", "1\n");
        let before = head(&dir);
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            commit(&dir, "second", false).unwrap();
        });
        assert_eq!(e.refs.len(), 1);
        assert_eq!(e.refs[0].name, "refs/heads/main");
        assert_eq!(e.refs[0].before.as_deref(), Some(before.as_str()));
        assert_eq!(e.refs[0].after.as_deref(), Some(head(&dir).as_str()));
        assert_eq!(e.refs[0].after.as_ref().unwrap().len(), 40, "full ids are kept");

        assert_eq!(undo(&dir, &e).unwrap(), "Undid test action");
        assert_eq!(head(&dir), before);
        assert_eq!(index_text(&dir, "a.txt"), "2\n", "the commit's changes are staged again");
        assert_eq!(status(&dir).unwrap().staged.len(), 1);
        assert!(run(&dir, &["reflog", "-1", "--format=%gs", "main"]).unwrap().contains("Headway: undo test action"));

        // The very first commit: undoing it leaves an empty branch with the files staged.
        let dir2 = temp_repo("undo-first-commit");
        std::fs::write(dir2.join("f.txt"), "x\n").unwrap();
        stage(&dir2, &["f.txt".into()]).unwrap();
        let e = recorded(&dir2, Undoable::Refs, || {
            commit(&dir2, "first", false).unwrap();
        });
        undo(&dir2, &e).unwrap();
        assert!(run(&dir2, &["rev-parse", "-q", "--verify", "HEAD"]).is_err());
        assert_eq!(index_text(&dir2, "f.txt"), "x\n");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn undo_an_amend_brings_back_the_original_commit() {
        let dir = temp_repo("undo-amend");
        commit_file(&dir, "a.txt", "1\n");
        let original = head(&dir);
        std::fs::write(dir.join("b.txt"), "forgotten\n").unwrap();
        stage(&dir, &["b.txt".into()]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            commit(&dir, "write a.txt, and b", true).unwrap();
        });
        assert_ne!(head(&dir), original);
        undo(&dir, &e).unwrap();
        assert_eq!(head(&dir), original);
        assert_eq!(message_of(&dir, "HEAD"), "write a.txt");
        let st = status(&dir).unwrap();
        assert_eq!(st.staged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["b.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_a_hard_reset_restores_commits_and_uncommitted_work() {
        let dir = temp_repo("undo-reset-hard");
        commit_file(&dir, "a.txt", "1\n");
        let older = head(&dir);
        commit_file(&dir, "a.txt", "2\n");
        let newer = head(&dir);
        std::fs::write(dir.join("b.txt"), "staged\n").unwrap();
        stage(&dir, &["b.txt".into()]).unwrap();
        std::fs::write(dir.join("a.txt"), "2 and unstaged\n").unwrap();
        let e = recorded(&dir, Undoable::Tree { save_work: true }, || {
            run(&dir, &["reset", "--hard", &older]).unwrap();
        });
        assert!(e.saved_work.is_some());
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "1\n");

        // Changing a file the undo has to rewrite: refused, and nothing moves.
        std::fs::write(dir.join("a.txt"), "edited after the reset\n").unwrap();
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("would be overwritten"), "{err}");
        assert_eq!(head(&dir), older);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "edited after the reset\n");

        run(&dir, &["checkout", "--", "a.txt"]).unwrap();
        undo(&dir, &e).unwrap();
        assert_eq!(head(&dir), newer);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "2 and unstaged\n");
        assert_eq!(index_text(&dir, "b.txt"), "staged\n", "staged work is staged again");
        assert_eq!(index_text(&dir, "a.txt"), "2\n", "unstaged work stays unstaged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_a_mixed_reset_restores_the_staging_area_only() {
        let dir = temp_repo("undo-reset-mixed");
        commit_file(&dir, "a.txt", "1\n");
        let older = head(&dir);
        commit_file(&dir, "a.txt", "2\n");
        let newer = head(&dir);
        std::fs::write(dir.join("c.txt"), "staged\n").unwrap();
        stage(&dir, &["c.txt".into()]).unwrap();
        let e = recorded(&dir, Undoable::Index, || {
            run(&dir, &["reset", "--mixed", &older]).unwrap();
        });
        undo(&dir, &e).unwrap();
        assert_eq!(head(&dir), newer);
        let st = status(&dir).unwrap();
        assert_eq!(st.staged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["c.txt"]);
        assert!(st.unstaged.is_empty(), "{:?}", st.unstaged);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_a_branch_delete_and_a_stash_drop() {
        let dir = temp_repo("undo-branch-delete");
        commit_file(&dir, "a.txt", "1\n");
        run(&dir, &["checkout", "-q", "-b", "feature"]).unwrap();
        commit_file(&dir, "f.txt", "only here\n");
        let tip = head(&dir);
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            run(&dir, &["branch", "-D", "feature"]).unwrap();
        });
        assert_eq!(e.refs, [RefMove { name: "refs/heads/feature".into(), before: Some(tip.clone()), after: None, after_on_remote: false }]);
        undo(&dir, &e).unwrap();
        assert_eq!(run(&dir, &["rev-parse", "feature"]).unwrap().trim(), tip);

        // Undoing it twice: the branch exists again, so the second undo refuses.
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("A new feature has been created since"), "{err}");

        std::fs::write(dir.join("a.txt"), "half done\n").unwrap();
        run(&dir, &["stash", "push", "-m", "half done"]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            run(&dir, &["stash", "drop"]).unwrap();
        });
        assert_eq!(e.dropped_stashes.len(), 1);
        undo(&dir, &e).unwrap();
        let s = stashes(&dir).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].message.contains("half done"), "{}", s[0].message);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_a_line_discard_restores_the_exact_bytes() {
        let dir = temp_repo("undo-lines");
        let original = "line 1\nline 2\nline 3";
        std::fs::write(dir.join("f.txt"), original).unwrap();
        stage(&dir, &["f.txt".into()]).unwrap();
        commit(&dir, "first", false).unwrap();
        let edited = "line 1\nnew a\nnew b\nline 2\nline 3";
        std::fs::write(dir.join("f.txt"), edited).unwrap();
        let d = file_diff(&dir, "f.txt", false, false).unwrap().unwrap();
        let sel = changed_lines(&d, |l| l.text.starts_with("new b"));
        let e = recorded(&dir, Undoable::Files(vec!["f.txt".into()]), || {
            discard_lines(&dir, &d, &sel).unwrap();
        });
        assert_eq!(e.mode, UndoMode::Files);
        assert!(!std::fs::read_to_string(dir.join("f.txt")).unwrap().contains("new b"));
        undo(&dir, &e).unwrap();
        assert_eq!(std::fs::read(dir.join("f.txt")).unwrap(), edited.as_bytes());

        // Edited again after the discard: refused, file untouched.
        let e = recorded(&dir, Undoable::Files(vec!["f.txt".into()]), || {
            discard_lines(&dir, &d, &sel).unwrap();
        });
        std::fs::write(dir.join("f.txt"), "something else").unwrap();
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("f.txt has changed since"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "something else");

        // A discarded untracked file comes back byte for byte, CRLF endings and all.
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/todo.md"), "remember\r\nthis\r\n").unwrap();
        let fc = status(&dir).unwrap().unstaged.into_iter().find(|f| f.path == "notes/todo.md").unwrap();
        let e = recorded(&dir, Undoable::Files(vec![fc.path.clone()]), || {
            discard(&dir, &fc).unwrap();
        });
        assert!(!dir.join("notes/todo.md").exists());
        undo(&dir, &e).unwrap();
        assert_eq!(std::fs::read(dir.join("notes/todo.md")).unwrap(), b"remember\r\nthis\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn undo_a_discard_keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_repo("undo-exec");
        std::fs::write(dir.join("run.sh"), "echo hi\n").unwrap();
        std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let fc = status(&dir).unwrap().unstaged.into_iter().find(|f| f.path == "run.sh").unwrap();
        let e = recorded(&dir, Undoable::Files(vec!["run.sh".into()]), || {
            discard(&dir, &fc).unwrap();
        });
        undo(&dir, &e).unwrap();
        assert_ne!(std::fs::metadata(dir.join("run.sh")).unwrap().permissions().mode() & 0o111, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_refuses_when_things_moved_or_were_pushed() {
        let up = std::env::temp_dir().join(format!("headway-test-undo-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&up);
        std::fs::create_dir_all(&up).unwrap();
        run(&up, &["init", "-q", "--bare", "-b", "main"]).unwrap();
        let dir = temp_repo("undo-refuse");
        commit_file(&dir, "a.txt", "1\n");
        run(&dir, &["remote", "add", "origin", &up.display().to_string()]).unwrap();
        run(&dir, &["push", "-q", "-u", "origin", "main"]).unwrap();

        // Something else moved the branch afterwards.
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            commit(&dir, "second", false).unwrap();
        });
        commit_file(&dir, "b.txt", "later\n");
        let moved = head(&dir);
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("main has moved since"), "{err}");
        assert_eq!(head(&dir), moved);

        // Pushed since: taking the commit back would rewrite shared history.
        std::fs::write(dir.join("a.txt"), "3\n").unwrap();
        stage(&dir, &["a.txt".into()]).unwrap();
        let e = recorded(&dir, Undoable::Refs, || {
            commit(&dir, "third", false).unwrap();
        });
        run(&dir, &["push", "-q"]).unwrap();
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("pushed to origin/main since"), "{err}");

        // A conflicted merge in progress.
        let e = recorded(&dir, Undoable::Refs, || {
            run(&dir, &["branch", "side"]).unwrap();
        });
        std::fs::write(dir.join(run(&dir, &["rev-parse", "--git-path", "MERGE_HEAD"]).unwrap().trim()), format!("{}\n", head(&dir))).unwrap();
        let err = undo(&dir, &e).unwrap_err();
        assert!(err.contains("in progress"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&up);
    }

    #[test]
    fn undo_a_checkout_switches_back() {
        let dir = temp_repo("undo-checkout");
        commit_file(&dir, "a.txt", "1\n");
        run(&dir, &["checkout", "-q", "-b", "other"]).unwrap();
        commit_file(&dir, "a.txt", "other\n");
        run(&dir, &["checkout", "-q", "main"]).unwrap();
        std::fs::write(dir.join("notes.txt"), "untouched\n").unwrap();
        let e = recorded(&dir, Undoable::Tree { save_work: false }, || {
            run(&dir, &["checkout", "other"]).unwrap();
        });
        assert_eq!(e.head_before.branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(e.head_after.branch.as_deref(), Some("refs/heads/other"));
        undo(&dir, &e).unwrap();
        assert_eq!(run(&dir, &["symbolic-ref", "HEAD"]).unwrap().trim(), "refs/heads/main");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "1\n");
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "untouched\n");
        assert!(status(&dir).unwrap().staged.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_changed_means_nothing_to_undo() {
        let dir = temp_repo("undo-nothing");
        commit_file(&dir, "a.txt", "1\n");
        let prep = prepare_undo(&dir, &Undoable::Tree { save_work: true }).unwrap();
        run(&dir, &["reset", "--hard", "HEAD"]).unwrap();
        assert_eq!(finish_undo(&dir, prep, "reset").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reflog_lists_where_head_and_branches_have_been() {
        let dir = temp_repo("reflog");
        commit_file(&dir, "a.txt", "1\n");
        commit_file(&dir, "a.txt", "2\n");
        run(&dir, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        let log = reflog(&dir, "HEAD", 50).unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].action, "reset");
        assert!(log[0].message.starts_with("moving to HEAD~1"), "{}", log[0].message);
        assert_eq!(log[1].action, "commit");
        assert_eq!(log[1].message, "write a.txt");
        assert!(log[1].time > 1_600_000_000);
        assert_eq!(log[2].action, "commit (initial)");
        assert_eq!(reflog(&dir, "refs/heads/main", 1).unwrap().len(), 1);
        assert!(reflog(&dir, "refs/heads/nope", 5).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // Blame and file history

    /// Who last changed each line (1-based line → (summary, name in that commit, line there)).
    fn blame_lines(dir: &Path, rev: Option<&str>, path: &str, opts: &BlameOptions) -> Vec<(String, String, u32)> {
        let ignore = ignore_revs(dir);
        let mut ranges = Vec::new();
        let commits = blame(dir, rev, path, opts, ignore.as_ref(), |_, r| ranges.push(r)).unwrap();
        ranges.sort_by_key(|r| r.line);
        ranges
            .iter()
            .flat_map(|r| (0..r.count).map(move |k| (r, k)))
            .map(|(r, k)| {
                let c = &commits[r.commit];
                let who = if c.uncommitted() { "(uncommitted)".to_string() } else { c.summary.clone() };
                (who, r.path.clone(), r.orig_line + k)
            })
            .collect()
    }

    #[test]
    fn blame_parser_reads_incremental_output() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let zero = "0".repeat(40);
        let out = format!(
            "{b} 2 2 1\nauthor Bo\nauthor-mail <bo@example.com>\nauthor-time 1700000000\nauthor-tz +0000\nsummary Rename it\nprevious {a} old name.txt\nfilename new.txt\n\
             {a} 1 1 1\nauthor Ana\nauthor-mail <ana@example.com>\nauthor-time 1600000000\nsummary First\nboundary\nfilename old name.txt\n\
             {a} 3 3 2\nfilename old name.txt\n\
             {zero} 5 5 1\nauthor Not Committed Yet\nsummary Version of new.txt from new.txt\nfilename new.txt\n"
        );
        let mut p = BlameParser::default();
        let ranges: Vec<BlameRange> = out.lines().filter_map(|l| p.feed(l)).collect();
        assert_eq!(ranges.len(), 4);
        assert_eq!(p.commits.len(), 3);
        let renamed = &p.commits[ranges[0].commit];
        assert_eq!((renamed.author.as_str(), renamed.email.as_str(), renamed.time), ("Bo", "bo@example.com", 1_700_000_000));
        assert_eq!(renamed.previous, Some((a.clone(), "old name.txt".to_string())));
        assert!(!renamed.boundary);
        // The boundary mark comes only with a commit's first entry, but holds for all of them.
        assert_eq!(ranges[1].commit, ranges[2].commit);
        assert!(p.commits[ranges[2].commit].boundary);
        assert_eq!((ranges[2].line, ranges[2].count, ranges[2].path.as_str()), (3, 2, "old name.txt"));
        assert!(p.commits[ranges[3].commit].uncommitted());
        // Anything else is ignored.
        assert_eq!(p.feed("not a header"), None);
    }

    #[test]
    fn blame_follows_renames_and_marks_boundary_commits() {
        let dir = temp_repo("blame-rename");
        commit_file_with(&dir, "f.txt", "one\ntwo\nthree\n", "Add f");
        run(&dir, &["mv", "f.txt", "g.txt"]).unwrap();
        std::fs::write(dir.join("g.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        stage(&dir, &["g.txt".into()]).unwrap();
        commit(&dir, "Rename to g", false).unwrap();
        let opts = BlameOptions::default();
        let lines = blame_lines(&dir, Some("HEAD"), "g.txt", &opts);
        assert_eq!(
            lines,
            vec![
                ("Add f".into(), "f.txt".into(), 1),
                ("Rename to g".into(), "g.txt".into(), 2),
                ("Add f".into(), "f.txt".into(), 3),
                ("Rename to g".into(), "g.txt".into(), 4),
            ]
        );
        // The rename knows where to look next; the root commit is the boundary.
        let mut seen = Vec::new();
        let commits = blame(&dir, Some("HEAD"), "g.txt", &opts, None, |_, r| seen.push(r)).unwrap();
        let first = run(&dir, &["rev-parse", "HEAD~1"]).unwrap().trim().to_string();
        let rename = commits.iter().find(|c| c.summary == "Rename to g").unwrap();
        assert_eq!(rename.previous, Some((first.clone(), "f.txt".into())));
        let root = commits.iter().find(|c| c.hash == first).unwrap();
        assert!(root.boundary && root.previous.is_none());
        // Blaming the parent: the old name at the old commit.
        assert_eq!(blame_lines(&dir, Some(&first), "f.txt", &opts).len(), 3);
        // A range stops at a boundary commit even when the lines are older.
        let mut ranges = Vec::new();
        let head = run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let commits = blame(&dir, Some(&format!("{first}..{head}")), "g.txt", &opts, None, |_, r| ranges.push(r)).unwrap();
        assert!(commits.iter().any(|c| c.hash == first && c.boundary));
        // The text it numbers against, at a commit and in the working copy.
        assert_eq!(blame_text(&dir, Some("HEAD"), "g.txt").unwrap(), vec!["one", "TWO", "three", "four"]);
        std::fs::write(dir.join("g.txt"), "one\r\nTWO\r\nthree\r\nfour\r\nfive\r\n").unwrap();
        assert_eq!(blame_text(&dir, None, "g.txt").unwrap().last().unwrap(), "five");
        let wd = blame_lines(&dir, None, "g.txt", &opts);
        assert_eq!(wd.len(), 5);
        assert_eq!(wd[4].0, "(uncommitted)");
        std::fs::write(dir.join("pic.bin"), [0u8, 1, 2, 0]).unwrap();
        assert!(blame_text(&dir, None, "pic.bin").unwrap_err().contains("binary"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blame_options_skip_whitespace_and_listed_commits() {
        let dir = temp_repo("blame-options");
        commit_file_with(&dir, "a.rs", "fn a() {\nx();\n}\n", "Write a");
        commit_file_with(&dir, "a.rs", "fn a() {\n    x();\n}\n", "Reformat");
        let plain = BlameOptions { ignore_revs: false, ..Default::default() };
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &plain)[1].0, "Reformat");
        let ws = BlameOptions { ignore_whitespace: true, ..plain };
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &ws)[1].0, "Write a");

        // A .git-blame-ignore-revs file is honoured even without the setting.
        assert_eq!(ignore_revs(&dir), None);
        let fmt = run(&dir, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(dir.join(".git-blame-ignore-revs"), format!("# formatting\n{fmt}")).unwrap();
        let found = ignore_revs(&dir).unwrap();
        assert_eq!((found.configured, found.exists), (false, true));
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &BlameOptions::default())[1].0, "Write a");
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &plain)[1].0, "Reformat");

        // So is the setting; turning it off clears it.
        std::fs::rename(dir.join(".git-blame-ignore-revs"), dir.join("revs.txt")).unwrap();
        run(&dir, &["config", "blame.ignoreRevsFile", "revs.txt"]).unwrap();
        assert!(ignore_revs(&dir).unwrap().configured);
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &BlameOptions::default())[1].0, "Write a");
        assert_eq!(blame_lines(&dir, Some("HEAD"), "a.rs", &plain)[1].0, "Reformat");

        // A setting that names a missing file gets a plain explanation.
        run(&dir, &["config", "blame.ignoreRevsFile", "gone.txt"]).unwrap();
        let missing = ignore_revs(&dir).unwrap();
        assert!(!missing.exists);
        let err = blame(&dir, Some("HEAD"), "a.rs", &BlameOptions::default(), Some(&missing), |_, _| {}).unwrap_err();
        assert!(err.contains("gone.txt"), "{err}");

        // Moves: -M and -C reach git.
        let args = blame_args(None, "a.rs", &BlameOptions { moves: BlameMoves::Files, ..plain }, None);
        assert!(args.contains(&"-C".to_string()) && args.ends_with(&["--".to_string(), "a.rs".to_string()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_history_follows_a_rename_and_folders_do_not() {
        let dir = temp_repo("file-log");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        commit_file_with(&dir, "src/f.txt", "1\n2\n3\n", "Add f");
        commit_file_with(&dir, "other.txt", "x\n", "Unrelated");
        commit_file_with(&dir, "src/f.txt", "1\ntwo\n3\n", "Edit f");
        run(&dir, &["mv", "src/f.txt", "g.txt"]).unwrap();
        commit(&dir, "Move f to g", false).unwrap();
        commit_file_with(&dir, "g.txt", "1\ntwo\n3\n4\n", "Edit g");

        let log = file_log(&dir, "g.txt", false, 100).unwrap();
        let seen: Vec<(&str, &str)> = log.iter().map(|c| (c.summary.as_str(), c.path.as_str())).collect();
        assert_eq!(seen, vec![("Edit g", "g.txt"), ("Move f to g", "g.txt"), ("Edit f", "src/f.txt"), ("Add f", "src/f.txt")]);
        assert_eq!(log[1].change, Change::Renamed);
        assert_eq!(log[1].old_path.as_deref(), Some("src/f.txt"));
        assert_eq!(log[3].change, Change::Added);
        assert_eq!(log[0].author, "Test");

        // The diff of just this file, under both names for the rename.
        let files = commit_files(&dir, &log[1].hash, &["src/f.txt".into(), "g.txt".into()], false).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].change.clone(), files[0].old_path.as_deref()), (Change::Renamed, Some("src/f.txt")));
        let files = commit_files(&dir, &log[2].hash, &["src/f.txt".into()], false).unwrap();
        assert_eq!((files[0].added, files[0].removed), (1, 1));

        // A folder lists what happened inside it, without following anything out.
        let folder = file_log(&dir, "src", true, 100).unwrap();
        let seen: Vec<&str> = folder.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(seen, vec!["Move f to g", "Edit f", "Add f"]);
        assert_eq!(file_log(&dir, "g.txt", false, 2).unwrap().len(), 2);
        assert_eq!(tracked_files(&dir).unwrap(), vec!["g.txt", "other.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_history_shows_only_commits_that_touched_those_lines() {
        let dir = temp_repo("line-log");
        commit_file_with(&dir, "f.txt", "a\nb\nc\nd\n", "Add f");
        commit_file_with(&dir, "f.txt", "a\nB\nc\nd\n", "Change b");
        commit_file_with(&dir, "f.txt", "a\nB\nc\nD\n", "Change d");
        run(&dir, &["mv", "f.txt", "g.txt"]).unwrap();
        commit(&dir, "Rename", false).unwrap();
        let log = line_log(&dir, "HEAD", "g.txt", 2, 2, 50).unwrap();
        let seen: Vec<&str> = log.iter().map(|(c, _)| c.summary.as_str()).collect();
        assert_eq!(seen, vec!["Change b", "Add f"]);
        let (c, diff) = &log[0];
        assert_eq!(c.path, "f.txt");
        let diff = diff.as_ref().unwrap();
        assert!(diff.hunks[0].lines.iter().any(|l| l.kind == LineKind::Add && l.text == "B"));
        assert!(line_log(&dir, "HEAD", "g.txt", 9, 9, 50).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn working_copy_lines_map_back_to_the_commit() {
        let patch = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,2 +2,4 @@\n a\n+new1\n+new2\n b\n@@ -10,2 +11,0 @@\n-x\n-y\n";
        let f = &parse_patch(patch)[0];
        // Before any change, unchanged.
        assert_eq!(line_before(f, 1), 1);
        // Inside the first hunk: context keeps its old number, added lines take the one above.
        assert_eq!(line_before(f, 2), 2);
        assert_eq!(line_before(f, 3), 2);
        assert_eq!(line_before(f, 4), 2);
        assert_eq!(line_before(f, 5), 3);
        // Between the hunks: two lines further down than before.
        assert_eq!(line_before(f, 8), 6);
        assert_eq!(line_before(f, 11), 9);
        // After the second hunk removed two lines.
        assert_eq!(line_before(f, 12), 12);
        assert_eq!(line_before(f, 13), 13);
    }
}
