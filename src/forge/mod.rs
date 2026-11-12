//! GitHub and GitLab, through the user's own `gh` and `glab`.
//!
//! Headway stores no tokens and has no login screen: it runs the GitHub CLI and
//! the GitLab CLI the person already uses, so their logins (GitHub Enterprise and
//! self-managed GitLab included) just work. Every run goes through
//! [`git::exec_tool`], so it's in the command log, and neither tool is ever asked
//! for its token.
//!
//! - [`parse_remote_url`] and [`pick_remotes`]: which forge, host and project a
//!   repository's remotes point at.
//! - [`Forge`]: the operations, returning typed data. [`github`] and [`gitlab`]
//!   implement it over the CLIs, [`fake`] from fixture files.
//! - [`Forges`]: finds out which forge a host is and hands out a [`Forge`] for it.
//! - [`cache`]: the latest results, and when to ask again.
//!
//! Everything here blocks; call it from worker threads.

pub mod cache;
pub mod fake;
pub mod github;
pub mod gitlab;
pub mod status;

use crate::git::{self, Tool, ToolOutput};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Which forge

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    GitHub,
    GitLab,
}

impl ForgeKind {
    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "GitHub",
            ForgeKind::GitLab => "GitLab",
        }
    }

    pub fn tool(self) -> Tool {
        match self {
            ForgeKind::GitHub => Tool::Gh,
            ForgeKind::GitLab => Tool::Glab,
        }
    }

    /// What the CLI is called, for hints.
    pub fn cli_name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "GitHub CLI (gh)",
            ForgeKind::GitLab => "GitLab CLI (glab)",
        }
    }

    pub fn install_url(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "https://cli.github.com",
            ForgeKind::GitLab => "https://gitlab.com/gitlab-org/cli#installation",
        }
    }

    /// "pull requests" or "merge requests".
    pub fn requests(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "pull requests",
            ForgeKind::GitLab => "merge requests",
        }
    }

    /// The forge a host is without asking anyone, for the public sites.
    /// The ref on the forge that holds pull request `n`'s head commit.
    pub fn pr_head_ref(self, n: u64) -> String {
        match self {
            ForgeKind::GitHub => format!("refs/pull/{n}/head"),
            ForgeKind::GitLab => format!("refs/merge-requests/{n}/head"),
        }
    }

    pub fn known_host(host: &str) -> Option<ForgeKind> {
        match host {
            "github.com" | "ssh.github.com" | "www.github.com" => Some(ForgeKind::GitHub),
            "gitlab.com" | "altssh.gitlab.com" | "www.gitlab.com" => Some(ForgeKind::GitLab),
            _ => None,
        }
    }

    /// A guess from the name ("github.acme.com", "gitlab.example.org"), only to say
    /// which CLI to sign in with when neither is signed in to the host.
    pub fn guess(host: &str) -> Option<ForgeKind> {
        let first = host.split('.').next().unwrap_or("");
        if first.contains("gitlab") {
            Some(ForgeKind::GitLab)
        } else if first.contains("github") || first == "ghe" {
            Some(ForgeKind::GitHub)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Remote URLs

/// Where a remote URL points: `host` (with the web port, if one was given for
/// http(s)) and the project's path (`owner/repo`, or `group/sub/repo` on GitLab).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteUrl {
    pub host: String,
    pub path: String,
}

impl RemoteUrl {
    /// The owner or (sub)group: everything before the last `/`.
    pub fn owner(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(o, _)| o)
    }

    pub fn name(&self) -> &str {
        self.path.rsplit_once('/').map_or(&self.path, |(_, n)| n)
    }
}

/// Read a remote URL as git gives it (after `insteadOf` rewrites): https, ssh
/// (`git@host:owner/repo.git`, `ssh://git@host:port/owner/repo.git`), git://.
/// Local paths and `file://` aren't on a forge.
pub fn parse_remote_url(url: &str) -> Option<RemoteUrl> {
    let url = url.trim();
    let (host, port, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        let web = matches!(scheme.as_str(), "http" | "https");
        if !web && !matches!(scheme.as_str(), "ssh" | "git" | "git+ssh" | "ssh+git") {
            return None;
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = match host_port.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, p.parse::<u16>().ok()),
            _ => (host_port, None),
        };
        // An ssh port is for ssh, not for the forge's web API.
        (host, if web { port } else { None }, path)
    } else {
        // scp-like: [user@]host:path. A drive letter ("C:\x") or a "/" before the ":" is a path.
        let (before, path) = url.split_once(':')?;
        if before.contains('/') || before.contains('\\') || before.len() < 2 {
            return None;
        }
        let host = before.rsplit_once('@').map_or(before, |(_, h)| h);
        (host, None, path)
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty() || host == "localhost" {
        return None;
    }
    let mut path = path.trim_matches('/');
    if let Some(p) = path.strip_suffix(".git") {
        path = p.trim_end_matches('/');
    }
    let path = path.trim_start_matches("~/").trim_start_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return None;
    }
    let host = match ForgeKind::known_host(&host) {
        Some(ForgeKind::GitHub) => "github.com".to_string(),
        Some(ForgeKind::GitLab) => "gitlab.com".to_string(),
        None => match port {
            Some(p) => format!("{host}:{p}"),
            None => host,
        },
    };
    Some(RemoteUrl { host, path: segments.join("/") })
}

/// The remotes a repository's pull requests go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRemotes {
    /// Where pull requests are opened: `upstream` (a fork's original), else
    /// `origin`, else the only remote.
    pub pr_remote: String,
    pub pr_url: RemoteUrl,
    /// Where branches are pushed, when that's elsewhere (a fork's own copy).
    pub push: Option<(String, RemoteUrl)>,
}

impl RepoRemotes {
    pub fn key(&self) -> RepoKey {
        RepoKey { host: self.pr_url.host.clone(), path: self.pr_url.path.clone() }
    }

    /// Where a new pull request's branch is pushed, and the fork it's then in
    /// (`owner/repo`; `None` when that's the project itself). A fork's pull
    /// requests go from origin's branch into upstream.
    pub fn head_target(&self) -> (&str, Option<&str>) {
        match &self.push {
            Some((name, url)) => (name, Some(&url.path)),
            None => (&self.pr_remote, None),
        }
    }
}

/// Choose the pull request remote (and the push remote) from `(name, url)` pairs.
pub fn pick_remotes(urls: &[(String, String)]) -> Option<RepoRemotes> {
    let parsed: Vec<(&str, RemoteUrl)> = urls.iter().filter_map(|(n, u)| parse_remote_url(u).map(|p| (n.as_str(), p))).collect();
    let find = |name: &str| parsed.iter().find(|(n, _)| *n == name).cloned();
    let only = (urls.len() == 1).then(|| parsed.first().cloned()).flatten();
    let (pr_remote, pr_url) = find("upstream").or_else(|| find("origin")).or(only)?;
    let push = find("origin").filter(|(n, u)| *n != pr_remote && *u != pr_url).map(|(n, u)| (n.to_string(), u));
    Some(RepoRemotes { pr_remote: pr_remote.to_string(), pr_url, push })
}

/// A project on a forge host: what the cache is keyed by. Worktrees and clones of
/// one project share it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoKey {
    pub host: String,
    pub path: String,
}

impl std::fmt::Display for RepoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.host, self.path)
    }
}

// ---------------------------------------------------------------------------
// What forges tell us

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Passing,
    Failing,
    Pending,
    /// No checks, or only skipped and neutral ones.
    #[default]
    None,
}

/// How a pull request's checks are doing: how many are in each state, and overall.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckSummary {
    pub passing: u32,
    pub failing: u32,
    pub pending: u32,
    /// Skipped, neutral or cancelled-by-design checks.
    pub none: u32,
    pub state: CheckState,
}

impl CheckSummary {
    /// Count one check in each of `states`, then work out the overall state:
    /// any failure fails, else anything still running is pending.
    pub fn from_states(states: impl IntoIterator<Item = (CheckState, u32)>) -> Self {
        let mut s = CheckSummary::default();
        for (state, n) in states {
            match state {
                CheckState::Passing => s.passing += n,
                CheckState::Failing => s.failing += n,
                CheckState::Pending => s.pending += n,
                CheckState::None => s.none += n,
            }
        }
        s.state = if s.failing > 0 {
            CheckState::Failing
        } else if s.pending > 0 {
            CheckState::Pending
        } else if s.passing > 0 {
            CheckState::Passing
        } else {
            CheckState::None
        };
        s
    }
}

/// Whether a pull request can be merged as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mergeable {
    Clean,
    Conflicts,
    /// Can't be merged yet (reviews, checks or branch rules).
    Blocked,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub author: String,
    pub head_branch: String,
    pub base_branch: String,
    /// The fork the branch is in (`owner/repo`), when it isn't the project itself.
    #[serde(default)]
    pub head_repo: Option<String>,
    pub url: String,
    #[serde(default)]
    pub review: ReviewDecision,
    #[serde(default)]
    pub requested_reviewers: Vec<String>,
    #[serde(default)]
    pub checks: CheckSummary,
    #[serde(default)]
    pub mergeable: Mergeable,
    /// Unix seconds.
    pub updated: i64,
    /// The commit the pull request's branch was at (for a merged one: what was merged).
    #[serde(default)]
    pub head_oid: Option<String>,
}

/// One check (a GitHub check run or status, a GitLab job).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub url: Option<String>,
    pub started: Option<i64>,
    pub finished: Option<i64>,
    /// The job or run id, to rerun it.
    pub id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub default_branch: String,
    pub merge_methods: Vec<MergeMethod>,
    pub web_url: String,
}

/// Pull requests per branch name (newest first), and when to slow down: the time
/// (Unix seconds) the forge's request allowance comes back, when it's nearly used up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchPrs {
    pub by_branch: HashMap<String, Vec<PullRequest>>,
    pub slow_down_until: Option<i64>,
}

/// The pull request lists in a repository's sidebar: open ones by me, open ones
/// asking me for a review, and every open one (each newest first, at most a page).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrLists {
    pub mine: Vec<PullRequest>,
    pub review_requested: Vec<PullRequest>,
    pub open: Vec<PullRequest>,
    /// How many are open in all (`open` holds the first page).
    pub open_total: usize,
    /// As in [`BranchPrs`]: when the request allowance comes back, if nearly used up.
    pub slow_down_until: Option<i64>,
}

/// Pull requests listed per page.
pub const LIST_PAGE: usize = 50;

/// Which pull requests to list.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub struct PrFilter {
    pub state: Option<PrState>,
    pub author: Option<String>,
    pub review_requested: Option<String>,
    pub limit: usize,
}

#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub enum CheckTarget {
    Pr(u64),
    Commit(String),
}

/// A pull request to open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewPr {
    /// The branch name on the forge.
    pub head: String,
    /// The fork it's in (`owner/repo`), when it isn't the project itself.
    pub head_repo: Option<String>,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    /// GitLab: delete the source branch once merged.
    pub remove_source_branch: bool,
    /// GitLab: squash the commits when merging.
    pub squash: bool,
}

/// What can be picked when opening a pull request: people (for reviewers and
/// assignees) and labels, sorted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrMeta {
    pub users: Vec<String>,
    pub labels: Vec<String>,
}

#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

/// Everything about one pull request, for its details view.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub struct PrDetails {
    pub pr: PullRequest,
    pub body: String,
    pub checks: Vec<Check>,
}


// ---------------------------------------------------------------------------
// Errors

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)] // NotYet: what the unfinished operations return.
pub enum ForgeError {
    /// The CLI isn't installed (or not on PATH).
    NotInstalled(ForgeKind),
    /// The CLI isn't signed in to this host.
    SignedOut { kind: ForgeKind, host: String },
    /// Too many requests; nothing more until `until` (Unix seconds), if it said.
    RateLimited { until: Option<i64> },
    NotFound,
    /// Something a later version of Headway will do.
    NotYet(&'static str),
    Other(String),
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::NotInstalled(k) => write!(f, "The {} isn't installed", k.cli_name()),
            ForgeError::SignedOut { kind, host } => write!(f, "{} isn't signed in to {host}", kind.tool().program()),
            ForgeError::RateLimited { .. } => write!(f, "Too many requests for now; trying again later"),
            ForgeError::NotFound => write!(f, "Not found (or you can't see it)"),
            ForgeError::NotYet(what) => write!(f, "Headway can't {what} yet"),
            ForgeError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub type ForgeResult<T> = Result<T, ForgeError>;

// ---------------------------------------------------------------------------
// The operations

/// One forge host, reached through its CLI (or fixtures). Operations not
/// written yet say "not yet".
#[allow(dead_code)] // Ahead of the UI: the pull request features to come use it.
pub trait Forge: Send + Sync {
    fn kind(&self) -> ForgeKind;
    /// The signed-in user's login.
    fn whoami(&self) -> ForgeResult<String>;
    fn repo_info(&self, repo: &RepoKey) -> ForgeResult<RepoInfo>;
    /// The pull requests whose head is each of `branches`, newest first, in as few
    /// calls as the forge allows.
    fn prs_for_branches(&self, repo: &RepoKey, branches: &[String]) -> ForgeResult<BranchPrs>;

    fn list_prs(&self, _repo: &RepoKey, _filter: &PrFilter) -> ForgeResult<Vec<PullRequest>> {
        Err(ForgeError::NotYet("list pull requests"))
    }
    /// The sidebar's lists, for the user `me`, `limit` per list.
    fn pr_lists(&self, _repo: &RepoKey, _me: &str, _limit: usize) -> ForgeResult<PrLists> {
        Err(ForgeError::NotYet("list pull requests"))
    }
    fn pr_details(&self, _repo: &RepoKey, _number: u64) -> ForgeResult<PrDetails> {
        Err(ForgeError::NotYet("show pull request details"))
    }
    fn checks(&self, _repo: &RepoKey, _target: &CheckTarget) -> ForgeResult<Vec<Check>> {
        Err(ForgeError::NotYet("list checks"))
    }
    /// Open a pull request; `me` is its author (the signed-in user).
    fn create_pr(&self, _repo: &RepoKey, _pr: &NewPr, _me: &str) -> ForgeResult<PullRequest> {
        Err(ForgeError::NotYet("create pull requests"))
    }
    /// The people and labels a new pull request can name.
    fn pr_meta(&self, _repo: &RepoKey) -> ForgeResult<PrMeta> {
        Err(ForgeError::NotYet("list reviewers and labels"))
    }
    fn merge(&self, _repo: &RepoKey, _number: u64, _method: MergeMethod) -> ForgeResult<()> {
        Err(ForgeError::NotYet("merge pull requests"))
    }
    fn close(&self, _repo: &RepoKey, _number: u64) -> ForgeResult<()> {
        Err(ForgeError::NotYet("close pull requests"))
    }
    fn mark_ready(&self, _repo: &RepoKey, _number: u64) -> ForgeResult<()> {
        Err(ForgeError::NotYet("mark pull requests ready"))
    }
    fn comment(&self, _repo: &RepoKey, _number: u64, _body: &str) -> ForgeResult<()> {
        Err(ForgeError::NotYet("comment on pull requests"))
    }
    fn review(&self, _repo: &RepoKey, _number: u64, _verdict: Verdict, _body: &str) -> ForgeResult<()> {
        Err(ForgeError::NotYet("review pull requests"))
    }
    fn rerun_failed_checks(&self, _repo: &RepoKey, _number: u64) -> ForgeResult<()> {
        Err(ForgeError::NotYet("rerun checks"))
    }
}

/// What a host is, as far as the CLIs know.
#[derive(Clone, Debug, PartialEq)]
pub struct HostKind {
    /// `None`: neither CLI knows the host, and its name doesn't say.
    pub kind: Option<ForgeKind>,
    /// Detection already found the CLI signed in (so `whoami` will work).
    pub signed_in: bool,
}

/// Finds out which forge a host is and hands out [`Forge`]s.
/// `dir`: the repository asking (for the command log).
pub trait Forges: Send + Sync {
    fn detect(&self, host: &str, dir: Option<&Path>) -> HostKind;
    fn forge(&self, kind: ForgeKind, host: &str, dir: Option<&Path>) -> Arc<dyn Forge>;
}

// ---------------------------------------------------------------------------
// Running the CLIs

/// Runs gh or glab. The real one goes through [`git::exec_tool`] (and so the
/// command log); tests use a stand-in with recorded answers.
pub trait Cli: Send + Sync {
    fn run(&self, tool: Tool, dir: Option<&Path>, args: &[&str], writes: bool) -> std::io::Result<ToolOutput>;
}

pub struct SystemCli;

impl Cli for SystemCli {
    fn run(&self, tool: Tool, dir: Option<&Path>, args: &[&str], writes: bool) -> std::io::Result<ToolOutput> {
        git::exec_tool(tool, dir, args, writes, git::TOOL_TIMEOUT)
    }
}

/// Run a read-only CLI call and parse what it printed as JSON. Errors are read
/// from what the CLI said, so they come out as typed [`ForgeError`]s.
pub fn run_json<T: serde::de::DeserializeOwned>(cli: &dyn Cli, kind: ForgeKind, host: &str, dir: Option<&Path>, args: &[&str]) -> ForgeResult<T> {
    let out = cli.run(kind.tool(), dir, args, false).map_err(|e| io_error(kind, &e))?;
    if out.code == Some(0) {
        return serde_json::from_slice(&out.stdout).map_err(|e| ForgeError::Other(format!("{} printed something Headway can't read: {e}", kind.tool().program())));
    }
    Err(classify_failure(kind, host, &out))
}

/// Run a CLI call that changes something on the forge; returns what it printed.
pub fn run_write(cli: &dyn Cli, kind: ForgeKind, host: &str, dir: Option<&Path>, args: &[&str]) -> ForgeResult<String> {
    let out = cli.run(kind.tool(), dir, args, true).map_err(|e| io_error(kind, &e))?;
    if out.code == Some(0) {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(classify_failure(kind, host, &out))
}

/// The number at the end of a pull request's address in what `gh pr create` or
/// `glab mr create` printed (".../pull/124", ".../-/merge_requests/12").
pub fn created_url(out: &str, kind: ForgeKind) -> Option<(String, u64)> {
    let marker = match kind {
        ForgeKind::GitHub => "/pull/",
        ForgeKind::GitLab => "/merge_requests/",
    };
    out.split_whitespace().filter(|w| w.starts_with("http")).find_map(|w| {
        let (_, rest) = w.rsplit_once(marker)?;
        let n: u64 = rest.trim_end_matches('/').parse().ok()?;
        Some((w.trim_end_matches('/').to_string(), n))
    })
}

/// The pull request just opened, as far as Headway can tell without asking again.
pub fn created_pr(new: &NewPr, me: &str, url: String, number: u64, now: i64) -> PullRequest {
    PullRequest {
        number,
        title: new.title.clone(),
        state: if new.draft { PrState::Draft } else { PrState::Open },
        author: me.to_string(),
        head_branch: new.head.clone(),
        base_branch: new.base.clone(),
        head_repo: new.head_repo.clone(),
        url,
        review: ReviewDecision::None,
        requested_reviewers: new.reviewers.clone(),
        checks: CheckSummary::default(),
        mergeable: Mergeable::Unknown,
        updated: now,
        head_oid: None,
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn io_error(kind: ForgeKind, e: &std::io::Error) -> ForgeError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ForgeError::NotInstalled(kind),
        _ => ForgeError::Other(format!("Couldn't start {}: {e}", kind.tool().program())),
    }
}

/// What a failed gh or glab run means, from what it printed.
pub fn classify_failure(kind: ForgeKind, host: &str, out: &ToolOutput) -> ForgeError {
    let program = kind.tool().program();
    if out.code.is_none() {
        return ForgeError::Other(format!("{program} took longer than {} s and was stopped", git::TOOL_TIMEOUT.as_secs()));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = format!("{stderr}\n{stdout}").to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
    if has(&["rate limit", "rate_limited", "429", "too many requests", "secondary rate"]) {
        ForgeError::RateLimited { until: None }
    } else if has(&["auth login", "not logged", "not authenticated", "no token", "401", "bad credentials", "authentication", "unauthorized", "has not been authenticated"]) {
        ForgeError::SignedOut { kind, host: host.to_string() }
    } else if has(&["404", "not found", "could not resolve to a repository", "does not exist"]) {
        ForgeError::NotFound
    } else {
        let first = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
        ForgeError::Other(if first.is_empty() { format!("{program} failed") } else { crate::cmdlog::redact(&first) })
    }
}

/// The real thing: `gh` and `glab`.
pub struct CliForges {
    pub cli: Arc<dyn Cli>,
}

impl CliForges {
    pub fn system() -> Self {
        CliForges { cli: Arc::new(SystemCli) }
    }

    fn signed_in(&self, tool: Tool, host: &str, dir: Option<&Path>) -> Option<bool> {
        match self.cli.run(tool, dir, &["auth", "status", "--hostname", host], false) {
            Ok(out) => Some(out.code == Some(0)),
            Err(_) => None,
        }
    }
}

impl Forges for CliForges {
    /// github.com and gitlab.com are known. For other hosts, ask which CLI is
    /// signed in to it: gh first, then glab.
    fn detect(&self, host: &str, dir: Option<&Path>) -> HostKind {
        if let Some(kind) = ForgeKind::known_host(host) {
            return HostKind { kind: Some(kind), signed_in: false };
        }
        if self.signed_in(Tool::Gh, host, dir) == Some(true) {
            return HostKind { kind: Some(ForgeKind::GitHub), signed_in: true };
        }
        if self.signed_in(Tool::Glab, host, dir) == Some(true) {
            return HostKind { kind: Some(ForgeKind::GitLab), signed_in: true };
        }
        HostKind { kind: ForgeKind::guess(host), signed_in: false }
    }

    fn forge(&self, kind: ForgeKind, host: &str, dir: Option<&Path>) -> Arc<dyn Forge> {
        let (cli, host, dir) = (self.cli.clone(), host.to_string(), dir.map(Path::to_path_buf));
        match kind {
            ForgeKind::GitHub => Arc::new(github::GitHub { cli, host, dir }),
            ForgeKind::GitLab => Arc::new(gitlab::GitLab { cli, host, dir }),
        }
    }
}

/// No forge anywhere (tests that don't ask for one): every host is unknown.
pub struct NoForges;

impl Forges for NoForges {
    fn detect(&self, _host: &str, _dir: Option<&Path>) -> HostKind {
        HostKind { kind: None, signed_in: false }
    }

    fn forge(&self, kind: ForgeKind, host: &str, _dir: Option<&Path>) -> Arc<dyn Forge> {
        // Never reached (nothing is detected); a forge that knows nothing, to be safe.
        Arc::new(fake::FakeForge { kind, host: host.into(), data: Default::default() })
    }
}

/// Fixtures for some folders, the real CLIs for the rest (development builds: the
/// sample projects use the fixtures the samples come with).
pub struct Routed {
    pub fixtures: Vec<(PathBuf, Arc<dyn Forges>)>,
    pub rest: Arc<dyn Forges>,
}

impl Routed {
    fn pick(&self, dir: Option<&Path>) -> &Arc<dyn Forges> {
        dir.and_then(|d| self.fixtures.iter().find(|(root, _)| d.starts_with(root))).map_or(&self.rest, |(_, f)| f)
    }
}

impl Forges for Routed {
    fn detect(&self, host: &str, dir: Option<&Path>) -> HostKind {
        self.pick(dir).detect(host, dir)
    }

    fn forge(&self, kind: ForgeKind, host: &str, dir: Option<&Path>) -> Arc<dyn Forge> {
        self.pick(dir).forge(kind, host, dir)
    }
}

/// The forges the app uses: the CLIs, except in development builds, where
/// `HEADWAY_FAKE_FORGE=<folder>` uses fixtures everywhere and the sample projects
/// always use theirs.
pub fn default_forges() -> Arc<dyn Forges> {
    let real: Arc<dyn Forges> = Arc::new(CliForges::system());
    if !cfg!(debug_assertions) {
        return real;
    }
    if let Some(dir) = std::env::var_os("HEADWAY_FAKE_FORGE").filter(|d| !d.is_empty()) {
        return Arc::new(fake::FakeForges::load(Path::new(&dir)));
    }
    match crate::samples::fake_forge() {
        Some((samples, data)) => Arc::new(Routed { fixtures: vec![(samples, Arc::new(fake::FakeForges::new(data)))], rest: real }),
        None => real,
    }
}

// ---------------------------------------------------------------------------
// What to tell the person

/// One line about a repository's forge for its ⋯ menu: who's signed in, or the
/// exact fix. Nothing at all while it's unknown or not a forge.
#[derive(Clone, Debug, PartialEq)]
pub enum StatusLine {
    SignedIn { kind: ForgeKind, user: String },
    Install(ForgeKind),
    SignIn { kind: ForgeKind, host: String },
}

impl StatusLine {
    pub fn text(&self) -> String {
        match self {
            StatusLine::SignedIn { kind, user } => format!("{} · signed in as {user} via {}", kind.name(), kind.tool().program()),
            StatusLine::Install(kind) => format!("Install the {} to see {}", kind.cli_name(), kind.requests()),
            StatusLine::SignIn { kind, .. } => format!("Run {} in a terminal to see {}", self.command().unwrap_or_default(), kind.requests()),
        }
    }

    /// The command to sign in, to copy.
    pub fn command(&self) -> Option<String> {
        match self {
            StatusLine::SignIn { kind, host } => Some(format!("{} auth login --hostname {host}", kind.tool().program())),
            _ => None,
        }
    }

    /// Where to get the CLI.
    pub fn link(&self) -> Option<&'static str> {
        match self {
            StatusLine::Install(kind) => Some(kind.install_url()),
            _ => None,
        }
    }
}

pub fn status_line(cache: &cache::ForgeCache, host: &str) -> Option<StatusLine> {
    let h = cache.host(host)?;
    let kind = h.kind?;
    match &h.connection {
        cache::Connection::SignedIn(user) => Some(StatusLine::SignedIn { kind, user: user.clone() }),
        cache::Connection::NotInstalled => Some(StatusLine::Install(kind)),
        cache::Connection::SignedOut => Some(StatusLine::SignIn { kind, host: host.to_string() }),
        cache::Connection::Unknown | cache::Connection::Failed(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Small shared helpers

/// RFC 3339 / ISO 8601 (`2026-11-10T09:30:12Z`, `…12.345+01:00`) to Unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let v = o[1..3].parse::<i64>().ok()? * 3600 + o[4..6].parse::<i64>().ok()? * 60;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

#[cfg(test)]
pub mod testing {
    //! A stand-in CLI: answers from a table, records what was asked.
    use super::*;
    use std::sync::Mutex;

    pub enum Answer {
        Ok(&'static str),
        Fail(i32, &'static str),
        Missing,
    }

    #[derive(Default)]
    pub struct StubCli {
        /// (tool, the arguments must start with these) → answer; the first match wins.
        pub answers: Vec<(Tool, Vec<&'static str>, Answer)>,
        pub calls: Mutex<Vec<(Tool, Vec<String>)>>,
    }

    impl StubCli {
        pub fn on(mut self, tool: Tool, args: &[&'static str], answer: Answer) -> Self {
            self.answers.push((tool, args.to_vec(), answer));
            self
        }

        pub fn calls(&self) -> Vec<(Tool, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Cli for StubCli {
        fn run(&self, tool: Tool, _dir: Option<&Path>, args: &[&str], _writes: bool) -> std::io::Result<ToolOutput> {
            self.calls.lock().unwrap().push((tool, args.iter().map(|s| s.to_string()).collect()));
            let hit = self.answers.iter().find(|(t, prefix, _)| *t == tool && args.len() >= prefix.len() && prefix.iter().zip(args).all(|(p, a)| p == a));
            match hit.map(|h| &h.2) {
                Some(Answer::Ok(out)) => Ok(ToolOutput { code: Some(0), stdout: out.as_bytes().to_vec(), stderr: vec![] }),
                Some(Answer::Fail(code, err)) => Ok(ToolOutput { code: Some(*code), stdout: vec![], stderr: err.as_bytes().to_vec() }),
                Some(Answer::Missing) => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found")),
                None => Ok(ToolOutput { code: Some(1), stdout: vec![], stderr: b"unexpected call".to_vec() }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Answer, StubCli};
    use super::*;

    fn url(host: &str, path: &str) -> Option<RemoteUrl> {
        Some(RemoteUrl { host: host.into(), path: path.into() })
    }

    #[test]
    fn remote_urls_name_a_host_and_project() {
        let table: &[(&str, Option<RemoteUrl>)] = &[
            ("https://github.com/cnu/headway.git", url("github.com", "cnu/headway")),
            ("https://github.com/cnu/headway", url("github.com", "cnu/headway")),
            ("https://github.com/cnu/headway/", url("github.com", "cnu/headway")),
            ("https://github.com/cnu/headway.git/", url("github.com", "cnu/headway")),
            ("https://ana:secret@GitHub.com/cnu/headway.git", url("github.com", "cnu/headway")),
            ("http://git.acme.dev:8080/team/app.git", url("git.acme.dev:8080", "team/app")),
            ("git@github.com:cnu/headway.git", url("github.com", "cnu/headway")),
            ("git@github.com:/cnu/headway.git", url("github.com", "cnu/headway")),
            ("github.com:cnu/headway", url("github.com", "cnu/headway")),
            ("ssh://git@github.com/cnu/headway.git", url("github.com", "cnu/headway")),
            // An ssh port belongs to ssh, not to the web host the CLIs talk to.
            ("ssh://git@gitlab.acme.dev:2222/team/app.git", url("gitlab.acme.dev", "team/app")),
            ("ssh://git@ssh.github.com:443/cnu/headway.git", url("github.com", "cnu/headway")),
            ("git+ssh://git@altssh.gitlab.com:443/grp/proj.git", url("gitlab.com", "grp/proj")),
            ("git://github.com/cnu/headway.git", url("github.com", "cnu/headway")),
            // GitLab subgroups keep every level.
            ("https://gitlab.com/group/sub/deeper/repo.git", url("gitlab.com", "group/sub/deeper/repo")),
            ("git@gitlab.example.org:group/sub/repo.git", url("gitlab.example.org", "group/sub/repo")),
            ("ssh://git@[2001:db8::1]:22/team/app.git", url("2001:db8::1", "team/app")),
            ("  https://github.com/cnu/headway.git\n", url("github.com", "cnu/headway")),
            // Not on a forge.
            ("/home/ana/repos/app.git", None),
            ("../app.git", None),
            ("C:\\repos\\app.git", None),
            ("C:/repos/app.git", None),
            ("file:///home/ana/app.git", None),
            ("https://github.com/cnu", None),
            ("https://github.com/", None),
            ("git@github.com:headway.git", None),
            ("ssh://localhost/team/app.git", None),
            ("", None),
        ];
        for (input, want) in table {
            assert_eq!(&parse_remote_url(input), want, "{input:?}");
        }
        let u = parse_remote_url("git@gitlab.com:group/sub/repo.git").unwrap();
        assert_eq!((u.owner(), u.name()), ("group/sub", "repo"));
    }

    #[test]
    fn insteadof_rewrites_are_followed() {
        let dir = std::env::temp_dir().join(format!("headway-test-forge-insteadof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git::run(&dir, &["init", "-q"]).unwrap();
        git::run(&dir, &["config", "url.git@github.com:.insteadOf", "gh:"]).unwrap();
        git::run(&dir, &["remote", "add", "origin", "gh:cnu/headway"]).unwrap();
        let remotes = pick_remotes(&git::remote_urls(&dir).unwrap()).unwrap();
        assert_eq!(remotes.pr_url, RemoteUrl { host: "github.com".into(), path: "cnu/headway".into() });
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(n, u)| (n.to_string(), u.to_string())).collect()
    }

    #[test]
    fn pull_requests_go_to_upstream_and_come_from_origin() {
        let fork = pick_remotes(&pairs(&[("origin", "git@github.com:ana/headway.git"), ("upstream", "https://github.com/cnu/headway.git")])).unwrap();
        assert_eq!((fork.pr_remote.as_str(), fork.pr_url.path.as_str()), ("upstream", "cnu/headway"));
        assert_eq!(fork.push.as_ref().map(|(n, u)| (n.as_str(), u.path.as_str())), Some(("origin", "ana/headway")));
        assert_eq!(fork.key(), RepoKey { host: "github.com".into(), path: "cnu/headway".into() });

        let plain = pick_remotes(&pairs(&[("origin", "https://github.com/cnu/headway.git"), ("backup", "/mnt/backup/headway.git")])).unwrap();
        assert_eq!((plain.pr_remote.as_str(), &plain.push), ("origin", &None));

        let only = pick_remotes(&pairs(&[("hub", "git@gitlab.com:grp/app.git")])).unwrap();
        assert_eq!(only.pr_remote, "hub");

        // A new pull request: pushed to the fork, opened against upstream.
        assert_eq!(fork.head_target(), ("origin", Some("ana/headway")));
        assert_eq!(plain.head_target(), ("origin", None));
        let same = pick_remotes(&pairs(&[("origin", "git@github.com:cnu/headway.git"), ("upstream", "https://github.com/cnu/headway")])).unwrap();
        assert_eq!(same.head_target(), ("upstream", None), "two names for one project aren't a fork");

        // Two remotes, neither called upstream or origin: no way to tell.
        assert_eq!(pick_remotes(&pairs(&[("a", "git@github.com:x/y.git"), ("b", "git@github.com:z/y.git")])), None);
        assert_eq!(pick_remotes(&pairs(&[("origin", "/srv/git/app.git")])), None);
        assert_eq!(pick_remotes(&[]), None);
        // An upstream on a plain path falls back to origin.
        let local_upstream = pick_remotes(&pairs(&[("upstream", "/srv/app.git"), ("origin", "git@github.com:ana/app.git")])).unwrap();
        assert_eq!(local_upstream.pr_remote, "origin");
    }

    #[test]
    fn hosts_are_detected_by_asking_which_cli_is_signed_in() {
        let cli = Arc::new(
            StubCli::default()
                .on(Tool::Gh, &["auth", "status", "--hostname", "github.acme.com"], Answer::Ok("github.acme.com\n  ✓ Logged in"))
                .on(Tool::Gh, &["auth", "status"], Answer::Fail(1, "You are not logged into any accounts on that host"))
                .on(Tool::Glab, &["auth", "status", "--hostname", "git.example.org"], Answer::Ok(""))
                .on(Tool::Glab, &["auth", "status"], Answer::Fail(1, "No token provided")),
        );
        let forges = CliForges { cli: cli.clone() };
        assert_eq!(forges.detect("github.com", None), HostKind { kind: Some(ForgeKind::GitHub), signed_in: false });
        assert_eq!(forges.detect("gitlab.com", None), HostKind { kind: Some(ForgeKind::GitLab), signed_in: false });
        assert!(cli.calls().is_empty(), "the public sites need no question");

        assert_eq!(forges.detect("github.acme.com", None), HostKind { kind: Some(ForgeKind::GitHub), signed_in: true });
        assert_eq!(forges.detect("git.example.org", None), HostKind { kind: Some(ForgeKind::GitLab), signed_in: true });
        // Neither: guess from the name for the sign-in hint, else nothing.
        assert_eq!(forges.detect("gitlab.corp.net", None), HostKind { kind: Some(ForgeKind::GitLab), signed_in: false });
        assert_eq!(forges.detect("code.corp.net", None), HostKind { kind: None, signed_in: false });
        let asked: Vec<String> = cli.calls().iter().map(|(t, a)| format!("{} {}", t.program(), a.join(" "))).collect();
        assert_eq!(asked[..2], ["gh auth status --hostname github.acme.com", "gh auth status --hostname git.example.org"]);
        assert_eq!(asked[2], "glab auth status --hostname git.example.org");
        assert!(asked.iter().all(|a| !a.contains("token")));
    }

    #[test]
    fn missing_cli_and_signed_out_come_back_as_such() {
        let missing = CliForges { cli: Arc::new(StubCli::default().on(Tool::Gh, &[], Answer::Missing)) };
        assert_eq!(missing.forge(ForgeKind::GitHub, "github.com", None).whoami(), Err(ForgeError::NotInstalled(ForgeKind::GitHub)));
        // Detection with gh missing still asks glab.
        let glab_only = CliForges { cli: Arc::new(StubCli::default().on(Tool::Gh, &[], Answer::Missing).on(Tool::Glab, &["auth", "status"], Answer::Ok(""))) };
        assert_eq!(glab_only.detect("git.corp", None).kind, Some(ForgeKind::GitLab));

        let out = CliForges { cli: Arc::new(StubCli::default().on(Tool::Gh, &["api", "user"], Answer::Fail(4, "To get started with GitHub CLI, please run:  gh auth login"))) };
        assert_eq!(out.forge(ForgeKind::GitHub, "github.com", None).whoami(), Err(ForgeError::SignedOut { kind: ForgeKind::GitHub, host: "github.com".into() }));
        let limited = CliForges { cli: Arc::new(StubCli::default().on(Tool::Glab, &[], Answer::Fail(1, "429 Too Many Requests"))) };
        assert_eq!(limited.forge(ForgeKind::GitLab, "gitlab.com", None).whoami(), Err(ForgeError::RateLimited { until: None }));
        let timed_out = classify_failure(ForgeKind::GitHub, "github.com", &ToolOutput { code: None, ..Default::default() });
        assert!(matches!(timed_out, ForgeError::Other(m) if m.contains("longer than 20 s")));
    }

    #[test]
    fn unfinished_operations_say_so() {
        let f = CliForges { cli: Arc::new(StubCli::default()) }.forge(ForgeKind::GitHub, "github.com", None);
        let key = RepoKey { host: "github.com".into(), path: "a/b".into() };
        assert_eq!(f.merge(&key, 1, MergeMethod::Squash), Err(ForgeError::NotYet("merge pull requests")));
        assert_eq!(f.merge(&key, 1, MergeMethod::Squash).unwrap_err().to_string(), "Headway can't merge pull requests yet");
    }

    /// The real `gh`, against GitHub (network; run by hand):
    /// `cargo test live_github -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_github() {
        let forges = CliForges::system();
        let f = forges.forge(ForgeKind::GitHub, "github.com", None);
        println!("signed in as {}", f.whoami().unwrap());
        let key = RepoKey { host: "github.com".into(), path: "cli/cli".into() };
        println!("{:?}", f.repo_info(&key).unwrap());
        let prs = f.prs_for_branches(&key, &["trunk".into(), "patch-1".into()]).unwrap();
        assert!(!prs.by_branch["patch-1"].is_empty());
        println!("{} pull requests from patch-1", prs.by_branch["patch-1"].len());
    }

    #[test]
    fn the_menu_line_names_the_user_or_the_fix() {
        let gh = ForgeKind::GitHub;
        assert_eq!(StatusLine::SignedIn { kind: gh, user: "cnu".into() }.text(), "GitHub · signed in as cnu via gh");
        let install = StatusLine::Install(gh);
        assert_eq!(install.text(), "Install the GitHub CLI (gh) to see pull requests");
        assert_eq!(install.link(), Some("https://cli.github.com"));
        let sign_in = StatusLine::SignIn { kind: ForgeKind::GitLab, host: "git.corp".into() };
        assert_eq!(sign_in.text(), "Run glab auth login --hostname git.corp in a terminal to see merge requests");
        assert_eq!(sign_in.command().as_deref(), Some("glab auth login --hostname git.corp"));
    }

    #[test]
    fn created_pull_requests_are_read_from_what_the_cli_printed() {
        let gh = "Creating pull request for ana:fix-login into main in cnu/headway\n\nhttps://github.com/cnu/headway/pull/124\n";
        assert_eq!(created_url(gh, ForgeKind::GitHub), Some(("https://github.com/cnu/headway/pull/124".into(), 124)));
        let glab = "!12 Fix login (fix-login)\n https://gitlab.example.org/group/sub/app/-/merge_requests/12\n";
        assert_eq!(created_url(glab, ForgeKind::GitLab).map(|u| u.1), Some(12));
        assert_eq!(created_url("Warning: 3 uncommitted changes", ForgeKind::GitHub), None);
        let new = NewPr { head: "fix-login".into(), base: "main".into(), title: "Fix login".into(), draft: true, reviewers: vec!["sam".into()], ..Default::default() };
        let pr = created_pr(&new, "ana", "u".into(), 124, 10);
        assert_eq!((pr.number, pr.state, pr.author.as_str(), pr.head_branch.as_str(), pr.requested_reviewers.len()), (124, PrState::Draft, "ana", "fix-login", 1));
    }

    #[test]
    fn times_are_read_in_any_offset() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-10T09:30:12Z"), Some(1_794_303_012));
        assert_eq!(parse_time("2026-11-10T09:30:12.123Z"), Some(1_794_303_012));
        assert_eq!(parse_time("2026-11-10T10:30:12+01:00"), Some(1_794_303_012));
        assert_eq!(parse_time("2026-11-10T04:00:12.5-05:30"), Some(1_794_303_012));
        assert_eq!(parse_time("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn check_summaries_fail_first_then_wait() {
        use CheckState::*;
        assert_eq!(CheckSummary::from_states([(Passing, 3), (Pending, 1)]).state, Pending);
        assert_eq!(CheckSummary::from_states([(Passing, 3), (Pending, 1), (Failing, 1)]).state, Failing);
        assert_eq!(CheckSummary::from_states([(Passing, 2), (None, 4)]), CheckSummary { passing: 2, none: 4, state: Passing, ..Default::default() });
        assert_eq!(CheckSummary::from_states([]).state, None);
    }
}
