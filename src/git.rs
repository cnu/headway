//! Thin, fast wrapper around the `git` command line.
//!
//! Everything here is blocking and is meant to be called from worker threads.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    Conflict,
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
            Change::Conflict => "!",
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
                if let Some(p) = fields.last() {
                    st.conflicts.push(FileChange { path: p.to_string(), old_path: None, change: Change::Conflict, submodule: false });
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

pub fn commit_detail(repo: &Path, hash: &str) -> GitResult<CommitDetail> {
    let head = run(repo, &["show", "-s", "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%at%x1f%P%x1f%B", hash])?;
    let f: Vec<&str> = head.splitn(7, '\x1f').collect();
    if f.len() < 7 {
        return Err("Unexpected output from git show".into());
    }
    let patch = run(
        repo,
        &["show", "--no-ext-diff", "--format=", "--patch", "-M", "--diff-merges=first-parent", hash],
    )?;
    Ok(CommitDetail {
        hash: f[0].to_string(),
        author: f[1].to_string(),
        email: f[2].to_string(),
        committer: f[3].to_string(),
        time: f[4].parse().unwrap_or(0),
        parents: f[5].split(' ').filter(|s| !s.is_empty()).map(String::from).collect(),
        message: f[6].trim_end().to_string(),
        files: parse_patch(&patch),
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

#[derive(Clone, Debug)]
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
        run_any(repo, &["diff", "--no-ext-diff", "--no-index", "--", "/dev/null", path])?
    } else if staged {
        run(repo, &["diff", "--no-ext-diff", "--cached", "-M", "--", path])?
    } else {
        run(repo, &["diff", "--no-ext-diff", "--", path])?
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
pub fn worktree_diffs(repo: &Path, st: &Status) -> GitResult<(Vec<DiffFile>, Vec<DiffFile>)> {
    let take = |out: String| -> HashMap<String, DiffFile> { parse_patch(&out).into_iter().map(|f| (f.path.clone(), f)).collect() };
    let mut unstaged_map = take(run(repo, &["diff", "--no-ext-diff"])?);
    let mut staged_map = take(run(repo, &["diff", "--no-ext-diff", "--cached", "-M"])?);
    let mut new_shown = 0;
    let unstaged = st
        .unstaged
        .iter()
        .map(|fc| {
            if fc.change != Change::Untracked {
                return unstaged_map.remove(&fc.path).unwrap_or_else(|| DiffFile::placeholder(fc, "No text changes"));
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
        .map(|fc| staged_map.remove(&fc.path).unwrap_or_else(|| DiffFile::placeholder(fc, "No text changes")))
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
        let detail = commit_detail(&dir, &commits[0].hash).unwrap();
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
        let (unstaged, staged) = worktree_diffs(&dir, &st).unwrap();
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
            conflicts: vec![fc("c", Change::Conflict)],
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
}
