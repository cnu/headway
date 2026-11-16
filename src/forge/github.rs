//! GitHub through `gh api` (GraphQL where one query can do the work of many).

use super::*;
use serde::Deserialize;

/// Branches asked about in one query (each is one aliased field).
const BRANCHES_PER_QUERY: usize = 30;
/// Below this many requests left, stop polling until the allowance comes back.
const LOW_RATE: i64 = 50;

pub struct GitHub {
    pub cli: Arc<dyn Cli>,
    pub host: String,
    pub dir: Option<PathBuf>,
}

impl GitHub {
    /// Run a gh command that changes something, keeping a refusal's own words.
    fn act(&self, args: &[String]) -> ForgeResult<String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_action(self.cli.as_ref(), ForgeKind::GitHub, &self.host, self.dir.as_deref(), &args)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> ForgeResult<T> {
        let mut all: Vec<&str> = args.to_vec();
        all.extend(["--hostname", self.host.as_str()]);
        run_json(self.cli.as_ref(), ForgeKind::GitHub, &self.host, self.dir.as_deref(), &all)
    }

    /// `gh api graphql` with string variables. GraphQL errors (and rate limits)
    /// become [`ForgeError`]s.
    fn graphql<T: serde::de::DeserializeOwned>(&self, query: &str, vars: &[(String, &str)]) -> ForgeResult<T> {
        let q = format!("query={query}");
        let fields: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut args = vec!["api", "graphql", "-f", q.as_str()];
        for f in &fields {
            args.extend(["-f", f.as_str()]);
        }
        let reply: GraphQl<T> = self.json(&args)?;
        graphql_result(reply)
    }
}

#[derive(Deserialize)]
struct GraphQl<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

fn graphql_result<T>(reply: GraphQl<T>) -> ForgeResult<T> {
    if let Some(e) = reply.errors.first() {
        return Err(match e.kind.as_str() {
            "RATE_LIMITED" => ForgeError::RateLimited { until: None },
            "NOT_FOUND" => ForgeError::NotFound,
            _ => ForgeError::Other(e.message.clone()),
        });
    }
    reply.data.ok_or_else(|| ForgeError::Other("GitHub sent an empty answer".into()))
}

impl Forge for GitHub {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitHub
    }

    fn whoami(&self) -> ForgeResult<String> {
        #[derive(Deserialize)]
        struct User {
            login: String,
        }
        Ok(self.json::<User>(&["api", "user"])?.login)
    }

    fn repo_info(&self, repo: &RepoKey) -> ForgeResult<RepoInfo> {
        let q = "query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { url defaultBranchRef { name } mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed } }";
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let data: RepoData = self.graphql(q, &[("owner".into(), url.owner()), ("name".into(), url.name())])?;
        parse_repo_info(data)
    }

    fn prs_for_branches(&self, repo: &RepoKey, branches: &[String]) -> ForgeResult<BranchPrs> {
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let mut all = BranchPrs::default();
        for chunk in branches.chunks(BRANCHES_PER_QUERY) {
            let mut vars: Vec<(String, &str)> = vec![("owner".into(), url.owner()), ("name".into(), url.name())];
            vars.extend(chunk.iter().enumerate().map(|(i, b)| (format!("h{i}"), b.as_str())));
            let data: PrsData = self.graphql(&prs_query(chunk.len()), &vars)?;
            let part = parse_prs(data, chunk)?;
            all.by_branch.extend(part.by_branch);
            all.slow_down_until = all.slow_down_until.max(part.slow_down_until);
        }
        Ok(all)
    }

    fn pr_lists(&self, repo: &RepoKey, _me: &str, limit: usize) -> ForgeResult<PrLists> {
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let mine = search_terms(repo, "author:@me");
        let review = search_terms(repo, "review-requested:@me");
        let vars = [("owner".to_string(), url.owner()), ("name".into(), url.name()), ("mine".into(), mine.as_str()), ("review".into(), review.as_str())];
        let data: ListsData = self.graphql(&lists_query(limit), &vars)?;
        parse_lists(data)
    }

    fn create_pr(&self, repo: &RepoKey, pr: &NewPr, me: &str) -> ForgeResult<PullRequest> {
        let args = create_args(repo, pr);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_write(self.cli.as_ref(), ForgeKind::GitHub, &self.host, self.dir.as_deref(), &args)?;
        let (url, number) = created_url(&out, ForgeKind::GitHub).ok_or_else(|| ForgeError::Other("gh didn't say which pull request it opened".into()))?;
        Ok(created_pr(pr, me, url, number, now_secs()))
    }

    fn pr_meta(&self, repo: &RepoKey) -> ForgeResult<PrMeta> {
        let q = "query($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { assignableUsers(first: 100) { nodes { login } } labels(first: 100, orderBy: {field: NAME, direction: ASC}) { nodes { name } } } }";
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let data: MetaData = self.graphql(q, &[("owner".into(), url.owner()), ("name".into(), url.name())])?;
        parse_meta(data)
    }

    fn pr_details(&self, repo: &RepoKey, number: u64) -> ForgeResult<PrDetails> {
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let data: DetailsData = self.graphql(&details_query(number), &[("owner".into(), url.owner()), ("name".into(), url.name())])?;
        parse_details(data)
    }

    fn pr_checks(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<Check>> {
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let data: ChecksData = self.graphql(&checks_query(number), &[("owner".into(), url.owner()), ("name".into(), url.name())])?;
        parse_checks(data)
    }

    fn conversation(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<PrComment>> {
        let url = RemoteUrl { host: repo.host.clone(), path: repo.path.clone() };
        let data: ConversationData = self.graphql(&conversation_query(number), &[("owner".into(), url.owner()), ("name".into(), url.name())])?;
        parse_conversation(data)
    }

    /// `gh run view <run> --log-failed`, cut to the failed job's lines.
    fn check_log(&self, repo: &RepoKey, check: &Check) -> ForgeResult<String> {
        let CheckKind::Actions { run, .. } = check.kind else { return Err(ForgeError::NotFound) };
        let args = log_args(repo, run);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = run_text(self.cli.as_ref(), ForgeKind::GitHub, &self.host, self.dir.as_deref(), &args)?;
        Ok(trim_log(&failed_job_log(&text, &check.name), LOG_LINES).0)
    }

    fn merge(&self, repo: &RepoKey, number: u64, req: &MergeRequest) -> ForgeResult<()> {
        self.act(&merge_args(repo, number, req)).map(drop)
    }

    fn set_state(&self, repo: &RepoKey, number: u64, change: StateChange) -> ForgeResult<()> {
        self.act(&state_args(repo, number, change)).map(drop)
    }

    fn comment(&self, repo: &RepoKey, number: u64, body: &str) -> ForgeResult<()> {
        self.act(&comment_args(repo, number, body)).map(drop)
    }

    /// A plain comment when there's nothing else to it; anything more is one
    /// review, posted with its line comments in a single call.
    fn review(&self, repo: &RepoKey, number: u64, review: &Review) -> ForgeResult<()> {
        if review.verdict == Verdict::Comment && review.comments.is_empty() {
            return self.comment(repo, number, &review.body);
        }
        let q = format!("query={}", review_mutation(review)?);
        self.act(&["api".into(), "graphql".into(), "-f".into(), q, "--hostname".into(), self.host.clone()]).map(drop)
    }

    /// `gh run rerun <run> --failed` for each workflow run with a failed job.
    fn rerun_failed_checks(&self, repo: &RepoKey, _number: u64, checks: &[Check]) -> ForgeResult<usize> {
        let all = rerun_commands(repo, checks);
        for args in &all {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run_write(self.cli.as_ref(), ForgeKind::GitHub, &self.host, self.dir.as_deref(), &args)?;
        }
        Ok(all.len())
    }
}

/// `-R` for gh's own commands: `HOST/OWNER/REPO` (they don't take `--hostname`).
fn repo_flag(repo: &RepoKey) -> String {
    format!("{}/{}", repo.host, repo.path)
}

/// `gh pr merge`, never asking anything. The method is always named, the
/// head commit pinned (so a push since isn't merged unseen), and `-R` keeps
/// gh from touching the local checkout (deleting or switching branches).
pub fn merge_args(repo: &RepoKey, number: u64, req: &MergeRequest) -> Vec<String> {
    let mut a: Vec<String> = vec!["pr".into(), "merge".into(), number.to_string(), "-R".into(), repo_flag(repo)];
    a.push(
        match req.method {
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
            MergeMethod::Merge | MergeMethod::FastForward => "--merge",
        }
        .into(),
    );
    if req.method == MergeMethod::Squash {
        if !req.title.trim().is_empty() {
            a.extend(["--subject".into(), req.title.trim().to_string()]);
        }
        a.extend(["--body".into(), req.message.trim().to_string()]);
    }
    if req.delete_branch {
        a.push("--delete-branch".into());
    }
    if req.auto {
        a.push("--auto".into());
    }
    if let Some(h) = &req.head {
        a.extend(["--match-head-commit".into(), h.clone()]);
    }
    a
}

/// `gh pr ready` (or `--undo`), `gh pr close`, `gh pr reopen`.
pub fn state_args(repo: &RepoKey, number: u64, change: StateChange) -> Vec<String> {
    let n = number.to_string();
    let words: &[&str] = match change {
        StateChange::Ready => &["pr", "ready", &n],
        StateChange::Draft => &["pr", "ready", &n, "--undo"],
        StateChange::Close => &["pr", "close", &n],
        StateChange::Reopen => &["pr", "reopen", &n],
    };
    let mut a: Vec<String> = words.iter().map(|w| w.to_string()).collect();
    a.extend(["-R".into(), repo_flag(repo)]);
    a
}

pub fn comment_args(repo: &RepoKey, number: u64, body: &str) -> Vec<String> {
    vec!["pr".into(), "comment".into(), number.to_string(), "--body".into(), body.to_string(), "-R".into(), repo_flag(repo)]
}

/// One `addPullRequestReview` with the verdict, the text and every line
/// comment (as review threads). Strings are written as JSON strings, which
/// GraphQL reads the same way.
pub fn review_mutation(r: &Review) -> ForgeResult<String> {
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let event = match r.verdict {
        Verdict::Comment => "COMMENT",
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Unapprove => return Err(ForgeError::NotYet("take back an approval on GitHub")),
    };
    if r.pr_id.is_empty() {
        return Err(ForgeError::Other("The pull request's details haven't been read yet".into()));
    }
    let threads: Vec<String> = r
        .comments
        .iter()
        .map(|c| {
            let side = match c.side {
                Side::Before => "LEFT",
                Side::After => "RIGHT",
            };
            let path = if c.side == Side::Before { c.old_path.as_deref().unwrap_or(&c.path) } else { &c.path };
            let start = if c.start < c.end { format!(", startLine: {}, startSide: {side}", c.start) } else { String::new() };
            format!("{{ path: {}, body: {}, line: {}, side: {side}{start} }}", q(path), q(&c.body), c.end)
        })
        .collect();
    Ok(format!(
        "mutation {{ addPullRequestReview(input: {{ pullRequestId: {}, event: {event}, body: {}, threads: [{}] }}) {{ pullRequestReview {{ url }} }} }}",
        q(&r.pr_id),
        q(&r.body),
        threads.join(", ")
    ))
}

/// `gh run view <run> --log-failed` for a workflow run.
pub fn log_args(repo: &RepoKey, run: u64) -> Vec<String> {
    ["run", "view", &run.to_string(), "--log-failed", "-R", &repo_flag(repo)].map(String::from).to_vec()
}

/// One `gh run rerun <run> --failed` per workflow run that has a failed job
/// (each run once, in order). Other checks can't be run again from here.
pub fn rerun_commands(repo: &RepoKey, checks: &[Check]) -> Vec<Vec<String>> {
    let mut runs: Vec<u64> = checks
        .iter()
        .filter(|c| c.state == CheckState::Failing)
        .filter_map(|c| match c.kind {
            CheckKind::Actions { run, .. } => Some(run),
            _ => None,
        })
        .collect();
    runs.sort_unstable();
    runs.dedup();
    runs.into_iter().map(|run| ["run", "rerun", &run.to_string(), "--failed", "-R", &repo_flag(repo)].map(String::from).to_vec()).collect()
}

/// What `gh run view --log-failed` printed for one job. Its lines are
/// `job<TAB>step<TAB>timestamp text`; this keeps `text`, with a heading at each
/// step. A job name that matches nothing keeps every job's lines.
pub fn failed_job_log(text: &str, job: &str) -> String {
    let rows: Vec<(&str, &str, &str)> = text
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            Some((f.next()?, f.next()?, f.next()?))
        })
        .collect();
    let mine: Vec<&(&str, &str, &str)> = rows.iter().filter(|r| r.0 == job).collect();
    let picked: Vec<&(&str, &str, &str)> = if mine.is_empty() { rows.iter().collect() } else { mine };
    if picked.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut step = None;
    for (j, s, rest) in picked {
        let heading = if *j == job { s.to_string() } else { format!("{j} › {s}") };
        if step.as_ref() != Some(&heading) {
            out += &format!("── {heading} ──\n");
            step = Some(heading);
        }
        // "2026-11-10T09:30:12.1234567Z text": drop the time.
        let line = match rest.split_once(' ') {
            Some((t, r)) if parse_time(t).is_some() => r,
            _ => rest,
        };
        out += line;
        out.push('\n');
    }
    out
}

/// `gh pr create` for `pr`. Every value is written `--flag=value`, so a title
/// starting with "-" stays a title.
pub fn create_args(repo: &RepoKey, pr: &NewPr) -> Vec<String> {
    let head = match &pr.head_repo {
        Some(fork) => format!("{}:{}", fork.split('/').next().unwrap_or(fork), pr.head),
        None => pr.head.clone(),
    };
    let mut a: Vec<String> = ["pr", "create"].map(String::from).to_vec();
    a.push(format!("--repo={}/{}", repo.host, repo.path));
    a.push(format!("--head={head}"));
    a.push(format!("--base={}", pr.base));
    a.push(format!("--title={}", pr.title));
    a.push(format!("--body={}", pr.body));
    if pr.draft {
        a.push("--draft".into());
    }
    for (flag, names) in [("reviewer", &pr.reviewers), ("assignee", &pr.assignees), ("label", &pr.labels)] {
        a.extend(names.iter().map(|n| format!("--{flag}={n}")));
    }
    a
}

#[derive(Deserialize)]
pub struct MetaData {
    repository: Option<MetaRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetaRepo {
    assignable_users: Connection<Login>,
    labels: Option<Connection<Name>>,
}

pub fn parse_meta(data: MetaData) -> ForgeResult<PrMeta> {
    let r = data.repository.ok_or(ForgeError::NotFound)?;
    let mut users: Vec<String> = r.assignable_users.nodes.into_iter().flatten().map(|u| u.login).collect();
    users.sort_by_key(|u| u.to_lowercase());
    let labels = r.labels.map(|l| l.nodes.into_iter().flatten().map(|n| n.name).collect()).unwrap_or_default();
    Ok(PrMeta { users, labels })
}

/// Everything the app reads about a pull request.
const PR_FRAGMENT: &str = "fragment pr on PullRequest { number title state isDraft url updatedAt headRefName headRefOid baseRefName isCrossRepository \
author { login } headRepository { nameWithOwner } reviewDecision mergeable mergeStateStatus \
reviewRequests(first: 20) { nodes { requestedReviewer { ... on User { login } ... on Team { slug } ... on Bot { login } } } } \
commits(last: 1) { nodes { commit { statusCheckRollup { state contexts { checkRunCountsByState { state count } statusContextCountsByState { state count } } } } } } }";

/// One query for the sidebar's three lists: two searches (by me, asking me for a
/// review) and the repository's open pull requests, `limit` each.
pub fn lists_query(limit: usize) -> String {
    let n = limit.clamp(1, 100);
    format!(
        "query($owner: String!, $name: String!, $mine: String!, $review: String!) {{ rateLimit {{ remaining resetAt }} \
mine: search(query: $mine, type: ISSUE, first: {n}) {{ nodes {{ ...pr }} }} \
review: search(query: $review, type: ISSUE, first: {n}) {{ nodes {{ ...pr }} }} \
repository(owner: $owner, name: $name) {{ pullRequests(states: OPEN, first: {n}, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ totalCount nodes {{ ...pr }} }} }} }} {PR_FRAGMENT}"
    )
}

/// The search terms for a list: open pull requests in `owner/name`, plus `who`.
pub fn search_terms(repo: &RepoKey, who: &str) -> String {
    format!("repo:{} is:pr is:open {who} sort:updated-desc", repo.path)
}

/// One query for `n` branches: an aliased `pullRequests(headRefName:)` per branch
/// (names go in as variables, never into the query text), plus the rate limit.
pub fn prs_query(n: usize) -> String {
    let vars: String = (0..n).map(|i| format!(", $h{i}: String!")).collect();
    let fields: String = (0..n)
        .map(|i| format!(" b{i}: pullRequests(headRefName: $h{i}, first: 5, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ nodes {{ ...pr }} }}"))
        .collect();
    format!("query($owner: String!, $name: String!{vars}) {{ rateLimit {{ remaining resetAt }} repository(owner: $owner, name: $name) {{{fields} }} }} {PR_FRAGMENT}")
}

// ---- Parsing (public to the tests, which feed recorded answers)

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoData {
    repository: Option<RepoNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoNode {
    url: String,
    default_branch_ref: Option<Name>,
    #[serde(default)]
    merge_commit_allowed: bool,
    #[serde(default)]
    squash_merge_allowed: bool,
    #[serde(default)]
    rebase_merge_allowed: bool,
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

pub fn parse_repo_info(data: RepoData) -> ForgeResult<RepoInfo> {
    let r = data.repository.ok_or(ForgeError::NotFound)?;
    let methods = [(r.merge_commit_allowed, MergeMethod::Merge), (r.squash_merge_allowed, MergeMethod::Squash), (r.rebase_merge_allowed, MergeMethod::Rebase)];
    Ok(RepoInfo {
        default_branch: r.default_branch_ref.map(|b| b.name).unwrap_or_default(),
        merge_methods: methods.iter().filter(|m| m.0).map(|m| m.1).collect(),
        web_url: r.url,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrsData {
    rate_limit: Option<RateLimit>,
    repository: Option<HashMap<String, Connection<PrNode>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListsData {
    rate_limit: Option<RateLimit>,
    mine: Option<Connection<PrNode>>,
    review: Option<Connection<PrNode>>,
    repository: Option<ListsRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListsRepo {
    pull_requests: Counted<PrNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counted<T> {
    #[serde(default)]
    total_count: usize,
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimit {
    remaining: i64,
    reset_at: String,
}

#[derive(Deserialize)]
struct Connection<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    url: String,
    updated_at: String,
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: Option<String>,
    base_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
    author: Option<Login>,
    head_repository: Option<NameWithOwner>,
    review_decision: Option<String>,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    review_requests: Option<Connection<ReviewRequest>>,
    commits: Option<Connection<CommitNode>>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameWithOwner {
    name_with_owner: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewRequest {
    requested_reviewer: Option<Reviewer>,
}

#[derive(Deserialize)]
struct Reviewer {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: CommitRollup,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitRollup {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    contexts: RollupContexts,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollupContexts {
    #[serde(default)]
    check_run_counts_by_state: Vec<StateCount>,
    #[serde(default)]
    status_context_counts_by_state: Vec<StateCount>,
}

#[derive(Deserialize)]
struct StateCount {
    state: String,
    count: u32,
}

/// A check run's or status's state, in four groups.
pub fn check_state(s: &str) -> CheckState {
    match s {
        "SUCCESS" => CheckState::Passing,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => CheckState::Failing,
        "PENDING" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" | "EXPECTED" => CheckState::Pending,
        _ => CheckState::None,
    }
}

pub fn parse_prs(data: PrsData, branches: &[String]) -> ForgeResult<BranchPrs> {
    let repo = data.repository.ok_or(ForgeError::NotFound)?;
    let slow_down_until = data.rate_limit.filter(|r| r.remaining < LOW_RATE).and_then(|r| parse_time(&r.reset_at));
    let mut by_branch = HashMap::new();
    for (i, branch) in branches.iter().enumerate() {
        let Some(conn) = repo.get(&format!("b{i}")) else { continue };
        let prs: Vec<PullRequest> = conn.nodes.iter().flatten().map(pr_from).collect();
        by_branch.insert(branch.clone(), prs);
    }
    Ok(BranchPrs { by_branch, slow_down_until })
}

pub fn parse_lists(data: ListsData) -> ForgeResult<PrLists> {
    let repo = data.repository.ok_or(ForgeError::NotFound)?;
    let read = |c: Option<Connection<PrNode>>| c.map(|c| c.nodes.iter().flatten().map(pr_from).collect()).unwrap_or_default();
    Ok(PrLists {
        mine: read(data.mine),
        review_requested: read(data.review),
        open: repo.pull_requests.nodes.iter().flatten().map(pr_from).collect(),
        open_total: repo.pull_requests.total_count,
        slow_down_until: data.rate_limit.filter(|r| r.remaining < LOW_RATE).and_then(|r| parse_time(&r.reset_at)),
    })
}

fn pr_from(n: &PrNode) -> PullRequest {
    let state = match n.state.as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ if n.is_draft => PrState::Draft,
        _ => PrState::Open,
    };
    let review = match n.review_decision.as_deref() {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    };
    let mergeable = match (n.mergeable.as_deref(), n.merge_state_status.as_deref()) {
        (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => Mergeable::Conflicts,
        (_, Some("BLOCKED" | "BEHIND" | "DRAFT" | "UNSTABLE")) => Mergeable::Blocked,
        (Some("MERGEABLE"), _) => Mergeable::Clean,
        _ => Mergeable::Unknown,
    };
    let rollup = n.commits.as_ref().and_then(|c| c.nodes.iter().flatten().last()).and_then(|c| c.commit.status_check_rollup.as_ref());
    let checks = rollup.map_or_else(CheckSummary::default, |r| {
        let counts = r.contexts.check_run_counts_by_state.iter().chain(&r.contexts.status_context_counts_by_state);
        CheckSummary::from_states(counts.map(|c| (check_state(&c.state), c.count)))
    });
    PullRequest {
        number: n.number,
        title: n.title.clone(),
        state,
        author: n.author.as_ref().map_or_else(|| "ghost".to_string(), |a| a.login.clone()),
        head_branch: n.head_ref_name.clone(),
        base_branch: n.base_ref_name.clone(),
        head_repo: n.is_cross_repository.then(|| n.head_repository.as_ref().map(|r| r.name_with_owner.clone())).flatten(),
        url: n.url.clone(),
        review,
        requested_reviewers: n
            .review_requests
            .iter()
            .flat_map(|c| c.nodes.iter().flatten())
            .filter_map(|r| r.requested_reviewer.as_ref().and_then(|w| w.login.clone().or_else(|| w.slug.clone())))
            .collect(),
        checks,
        mergeable,
        updated: parse_time(&n.updated_at).unwrap_or(0),
        head_oid: n.head_ref_oid.clone(),
    }
}

/// The pull request with its description, and what the viewer may do in the
/// repository. The number is written into the query (it's a number).
pub fn details_query(number: u64) -> String {
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ viewerPermission \
mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed autoMergeAllowed deleteBranchOnMerge \
pullRequest(number: {number}) {{ ...pr body id viewerDidAuthor autoMergeRequest {{ enabledAt }} }} }} }} {PR_FRAGMENT}"
    )
}

/// Every check on the pull request's latest commit: Actions jobs with their run,
/// other apps' checks, and commit statuses.
pub fn checks_query(number: u64) -> String {
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ pullRequest(number: {number}) {{ commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 100) {{ nodes {{ \
__typename ... on CheckRun {{ name status conclusion startedAt completedAt detailsUrl databaseId checkSuite {{ workflowRun {{ databaseId }} }} }} \
... on StatusContext {{ context state targetUrl createdAt }} }} }} }} }} }} }} }} }} }}"
    )
}

/// Comments, and reviews with their line comments.
pub fn conversation_query(number: u64) -> String {
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ pullRequest(number: {number}) {{ \
comments(first: 100) {{ nodes {{ author {{ login }} body createdAt url }} }} \
reviews(first: 100) {{ nodes {{ author {{ login }} state body submittedAt url comments(first: 100) {{ nodes {{ author {{ login }} body createdAt path line originalLine url }} }} }} }} }} }} }}"
    )
}

#[derive(Deserialize)]
pub struct DetailsData {
    repository: Option<DetailsRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailsRepo {
    viewer_permission: Option<String>,
    #[serde(default)]
    merge_commit_allowed: bool,
    #[serde(default)]
    squash_merge_allowed: bool,
    #[serde(default)]
    rebase_merge_allowed: bool,
    #[serde(default)]
    auto_merge_allowed: bool,
    #[serde(default)]
    delete_branch_on_merge: bool,
    pull_request: Option<DetailsPr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetailsPr {
    #[serde(flatten)]
    pr: PrNode,
    #[serde(default)]
    body: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    viewer_did_author: bool,
    auto_merge_request: Option<serde_json::Value>,
}

pub fn parse_details(data: DetailsData) -> ForgeResult<PrDetails> {
    let repo = data.repository.ok_or(ForgeError::NotFound)?;
    let p = repo.pull_request.ok_or(ForgeError::NotFound)?;
    let writer = matches!(repo.viewer_permission.as_deref(), Some("ADMIN" | "MAINTAIN" | "WRITE"));
    let methods = [(repo.merge_commit_allowed, MergeMethod::Merge), (repo.squash_merge_allowed, MergeMethod::Squash), (repo.rebase_merge_allowed, MergeMethod::Rebase)];
    let merge = MergeFacts {
        node_id: p.id,
        status: p.pr.merge_state_status.clone().unwrap_or_default(),
        viewer_is_author: p.viewer_did_author,
        viewer_approved: false,
        can_merge: writer,
        methods: methods.iter().filter(|m| m.0).map(|m| m.1).collect(),
        auto_merge_allowed: repo.auto_merge_allowed,
        auto_merge_on: p.auto_merge_request.is_some_and(|v| !v.is_null()),
        delete_branch: repo.delete_branch_on_merge,
        diff_refs: None,
    };
    Ok(PrDetails { pr: pr_from(&p.pr), body: p.body, can_rerun: writer, merge })
}

#[derive(Deserialize)]
pub struct ChecksData {
    repository: Option<ChecksRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChecksRepo {
    pull_request: Option<ChecksPr>,
}

#[derive(Deserialize)]
struct ChecksPr {
    commits: Connection<ChecksCommitNode>,
}

#[derive(Deserialize)]
struct ChecksCommitNode {
    commit: ChecksCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChecksCommit {
    status_check_rollup: Option<ChecksRollup>,
}

#[derive(Deserialize)]
struct ChecksRollup {
    contexts: Connection<Context>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Context {
    #[serde(rename = "__typename", default)]
    typename: String,
    // CheckRun
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    details_url: Option<String>,
    database_id: Option<u64>,
    check_suite: Option<CheckSuite>,
    // StatusContext
    context: Option<String>,
    state: Option<String>,
    target_url: Option<String>,
    created_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckSuite {
    workflow_run: Option<WorkflowRun>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRun {
    database_id: u64,
}

pub fn parse_checks(data: ChecksData) -> ForgeResult<Vec<Check>> {
    let pr = data.repository.and_then(|r| r.pull_request).ok_or(ForgeError::NotFound)?;
    let Some(rollup) = pr.commits.nodes.into_iter().flatten().last().and_then(|n| n.commit.status_check_rollup) else { return Ok(Vec::new()) };
    let time = |t: &Option<String>| t.as_deref().and_then(parse_time);
    Ok(rollup
        .contexts
        .nodes
        .into_iter()
        .flatten()
        .map(|c| {
            if c.typename == "StatusContext" {
                let state = check_state(c.state.as_deref().unwrap_or(""));
                return Check { name: c.context.unwrap_or_default(), state, url: c.target_url, started: time(&c.created_at), finished: None, kind: CheckKind::Other };
            }
            let state = match (c.status.as_deref(), c.conclusion.as_deref()) {
                (Some(s), _) if s != "COMPLETED" => CheckState::Pending,
                (_, Some(done)) => check_state(done),
                _ => CheckState::None,
            };
            let kind = match c.check_suite.and_then(|s| s.workflow_run) {
                Some(run) => CheckKind::Actions { run: run.database_id, job: c.database_id },
                None => CheckKind::Other,
            };
            Check { name: c.name.unwrap_or_default(), state, url: c.details_url, started: time(&c.started_at), finished: time(&c.completed_at), kind }
        })
        .collect())
}

#[derive(Deserialize)]
pub struct ConversationData {
    repository: Option<ConvRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvRepo {
    pull_request: Option<ConvPr>,
}

#[derive(Deserialize)]
struct ConvPr {
    comments: Connection<ConvComment>,
    reviews: Connection<ConvReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvComment {
    author: Option<Login>,
    #[serde(default)]
    body: String,
    created_at: String,
    url: Option<String>,
    path: Option<String>,
    line: Option<u32>,
    original_line: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvReview {
    author: Option<Login>,
    state: String,
    #[serde(default)]
    body: String,
    submitted_at: Option<String>,
    url: Option<String>,
    comments: Option<Connection<ConvComment>>,
}

fn login(a: &Option<Login>) -> String {
    a.as_ref().map_or_else(|| "ghost".to_string(), |a| a.login.clone())
}

/// Comments and reviews, oldest first. A review that only carries line comments
/// shows as those comments; unsubmitted (pending) reviews are left out.
pub fn parse_conversation(data: ConversationData) -> ForgeResult<Vec<PrComment>> {
    let pr = data.repository.and_then(|r| r.pull_request).ok_or(ForgeError::NotFound)?;
    let mut out: Vec<PrComment> = pr
        .comments
        .nodes
        .into_iter()
        .flatten()
        .map(|c| PrComment { kind: CommentKind::Comment, author: login(&c.author), time: parse_time(&c.created_at).unwrap_or(0), body: c.body, path: None, line: None, url: c.url })
        .collect();
    for r in pr.reviews.nodes.into_iter().flatten() {
        let kind = match r.state.as_str() {
            "PENDING" => continue,
            "APPROVED" => CommentKind::Approved,
            "CHANGES_REQUESTED" => CommentKind::ChangesRequested,
            _ => CommentKind::Reviewed,
        };
        let time = r.submitted_at.as_deref().and_then(parse_time).unwrap_or(0);
        if kind != CommentKind::Reviewed || !r.body.trim().is_empty() {
            out.push(PrComment { kind, author: login(&r.author), time, body: r.body, path: None, line: None, url: r.url });
        }
        for c in r.comments.into_iter().flat_map(|c| c.nodes).flatten() {
            out.push(PrComment {
                kind: CommentKind::Line,
                author: login(&c.author),
                time: parse_time(&c.created_at).unwrap_or(time),
                body: c.body,
                path: c.path,
                line: c.line.or(c.original_line),
                url: c.url,
            });
        }
    }
    Ok(in_time_order(out))
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Answer, StubCli};
    use super::*;

    const PRS: &str = include_str!("fixtures/gh-prs-for-branches.json");
    const REPO: &str = include_str!("fixtures/gh-repo-info.json");
    const USER: &str = include_str!("fixtures/gh-user.json");

    fn key() -> RepoKey {
        RepoKey { host: "github.com".into(), path: "cli/cli".into() }
    }

    #[test]
    fn recorded_pull_requests_are_read() {
        let branches: Vec<String> = include_str!("fixtures/gh-prs-for-branches.branches").lines().map(String::from).collect();
        let reply: GraphQl<PrsData> = serde_json::from_str(PRS).unwrap();
        let prs = parse_prs(graphql_result(reply).unwrap(), &branches).unwrap();
        assert_eq!(prs.by_branch.len(), branches.len(), "every branch has an answer, even an empty one");
        let all: Vec<&PullRequest> = prs.by_branch.values().flatten().collect();
        assert!(!all.is_empty());
        for pr in &all {
            assert!(pr.number > 0 && !pr.title.is_empty() && pr.url.starts_with("https://github.com/"), "{pr:?}");
            assert!(pr.updated > 1_600_000_000, "{pr:?}");
            assert!(branches.contains(&pr.head_branch));
            assert_eq!(pr.head_oid.as_ref().map(|o| o.len()), Some(40), "the commit it was at");
        }
        assert!(all.iter().any(|p| p.state == PrState::Merged), "the recording has a merged one");
        assert!(all.iter().any(|p| p.checks.passing > 0), "and one with checks");
        assert!(all.iter().any(|p| p.head_repo.is_some()), "and one from a fork");
        assert!(prs.by_branch.values().any(|v| v.is_empty()), "and a branch without one");
    }

    #[test]
    fn recorded_lists_are_read() {
        let reply: GraphQl<ListsData> = serde_json::from_str(include_str!("fixtures/gh-pr-lists.json")).unwrap();
        let lists = parse_lists(graphql_result(reply).unwrap()).unwrap();
        assert_eq!(lists.open.len(), 5);
        assert_eq!(lists.open_total, 59, "more than the page holds");
        assert!(lists.open.iter().any(|p| p.state == PrState::Draft) && lists.open.iter().any(|p| p.head_repo.is_some()));
        assert_eq!(lists.mine[0].author, "octo-user");
        assert!(lists.review_requested[0].requested_reviewers.contains(&"octo-user".to_string()));
        assert_eq!(lists.slow_down_until, None);
    }

    #[test]
    fn one_query_lists_all_three_with_the_search_terms_as_variables() {
        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["api", "graphql"], Answer::Ok(include_str!("fixtures/gh-pr-lists.json"))));
        let forge = GitHub { cli: cli.clone(), host: "github.com".into(), dir: None };
        forge.pr_lists(&key(), "octo-user", 50).unwrap();
        let calls = cli.calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0].1;
        assert!(args.contains(&"mine=repo:cli/cli is:pr is:open author:@me sort:updated-desc".to_string()), "{args:?}");
        assert!(args.contains(&"review=repo:cli/cli is:pr is:open review-requested:@me sort:updated-desc".to_string()));
        let query = args.iter().find(|a| a.starts_with("query=")).unwrap();
        assert!(query.contains("first: 50") && query.contains("totalCount") && query.contains("states: OPEN"));
    }

    #[test]
    fn recorded_repository_and_user_are_read() {
        let reply: GraphQl<RepoData> = serde_json::from_str(REPO).unwrap();
        let info = parse_repo_info(graphql_result(reply).unwrap()).unwrap();
        assert_eq!(info.default_branch, "trunk");
        assert_eq!(info.web_url, "https://github.com/cli/cli");
        assert!(info.merge_methods.contains(&MergeMethod::Squash));

        let forge = GitHub { cli: Arc::new(StubCli::default().on(Tool::Gh, &["api", "user"], Answer::Ok(USER))), host: "github.com".into(), dir: None };
        assert_eq!(forge.whoami().unwrap(), "octo-user");
    }

    #[test]
    fn one_query_covers_many_branches_and_names_stay_out_of_it() {
        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["api", "graphql"], Answer::Ok(r#"{"data":{"rateLimit":{"remaining":4000,"resetAt":"2026-11-10T10:00:00Z"},"repository":{"b0":{"nodes":[]},"b1":{"nodes":[]}}}}"#)));
        let forge = GitHub { cli: cli.clone(), host: "github.acme.com".into(), dir: None };
        let branches = vec!["feature/x".to_string(), "a\"b{c}".to_string()];
        let prs = forge.prs_for_branches(&RepoKey { host: "github.acme.com".into(), path: "team/app".into() }, &branches).unwrap();
        assert_eq!(prs.by_branch.len(), 2);
        assert_eq!(prs.slow_down_until, None);
        let calls = cli.calls();
        assert_eq!(calls.len(), 1, "one call for both branches");
        let args = &calls[0].1;
        assert!(args.windows(2).any(|w| w == ["--hostname", "github.acme.com"]), "Enterprise hosts are named: {args:?}");
        assert!(args.contains(&"h1=a\"b{c}".to_string()));
        let query = args.iter().find(|a| a.starts_with("query=")).unwrap();
        assert!(!query.contains("feature/x"), "branch names are variables");
        assert!(query.contains("statusCheckRollup") && query.contains("reviewDecision") && query.contains("rateLimit"));

        // 65 branches: three queries of at most 30.
        let many: Vec<String> = (0..65).map(|i| format!("b{i}")).collect();
        let _ = forge.prs_for_branches(&key(), &many);
        assert_eq!(cli.calls().len(), 4);
    }

    #[test]
    fn rate_limits_slow_down_and_stop() {
        let low = r#"{"data":{"rateLimit":{"remaining":3,"resetAt":"2026-11-10T10:00:00Z"},"repository":{"b0":{"nodes":[]}}}}"#;
        let reply: GraphQl<PrsData> = serde_json::from_str(low).unwrap();
        let prs = parse_prs(graphql_result(reply).unwrap(), &["x".into()]).unwrap();
        assert_eq!(prs.slow_down_until, parse_time("2026-11-10T10:00:00Z"));

        let limited = r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 1."}]}"#;
        let reply: GraphQl<PrsData> = serde_json::from_str(limited).unwrap();
        assert_eq!(graphql_result(reply).err(), Some(ForgeError::RateLimited { until: None }));

        let cli = Arc::new(StubCli::default().on(Tool::Gh, &[], Answer::Fail(1, "gh: API rate limit exceeded for user ID 1. (HTTP 403)")));
        let forge = GitHub { cli, host: "github.com".into(), dir: None };
        assert_eq!(forge.prs_for_branches(&key(), &["x".into()]).unwrap_err(), ForgeError::RateLimited { until: None });
    }

    #[test]
    fn creating_names_the_project_the_fork_and_every_option() {
        let new = NewPr {
            head: "fix-login".into(),
            head_repo: Some("ana/headway".into()),
            base: "main".into(),
            title: "-x looks like a flag".into(),
            body: "Line one\nLine two".into(),
            draft: true,
            reviewers: vec!["sam".into(), "jo".into()],
            assignees: vec!["ana".into()],
            labels: vec!["bug".into()],
            ..Default::default()
        };
        let printed = "Creating pull request for ana:fix-login into main in cnu/headway\n\nhttps://github.acme.com/cnu/headway/pull/124\n";
        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["pr", "create"], Answer::Ok(printed)));
        let forge = GitHub { cli: cli.clone(), host: "github.acme.com".into(), dir: None };
        let pr = forge.create_pr(&RepoKey { host: "github.acme.com".into(), path: "cnu/headway".into() }, &new, "ana").unwrap();
        assert_eq!((pr.number, pr.state, pr.url.as_str()), (124, PrState::Draft, "https://github.acme.com/cnu/headway/pull/124"));
        assert_eq!(pr.head_repo.as_deref(), Some("ana/headway"));
        let args = &cli.calls()[0].1;
        for want in ["--repo=github.acme.com/cnu/headway", "--head=ana:fix-login", "--base=main", "--title=-x looks like a flag", "--body=Line one\nLine two", "--draft", "--reviewer=sam", "--reviewer=jo", "--assignee=ana", "--label=bug"] {
            assert!(args.contains(&want.to_string()), "{want} in {args:?}");
        }
        let plain = create_args(&key(), &NewPr { head: "x".into(), base: "trunk".into(), ..Default::default() });
        assert!(plain.contains(&"--head=x".to_string()) && !plain.contains(&"--draft".to_string()));

        let refused = GitHub { cli: Arc::new(StubCli::default().on(Tool::Gh, &["pr", "create"], Answer::Fail(1, "pull request create failed: GraphQL: No commits between main and x"))), host: "github.com".into(), dir: None };
        let err = refused.create_pr(&key(), &new, "ana").unwrap_err();
        assert!(err.to_string().contains("No commits between"), "{err}");
    }

    #[test]
    fn reviewers_and_labels_come_in_one_query() {
        let reply = r#"{"data":{"repository":{"assignableUsers":{"nodes":[{"login":"sam"},{"login":"Ana"},{"login":"jo"}]},"labels":{"nodes":[{"name":"bug"},{"name":"docs"}]}}}}"#;
        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["api", "graphql"], Answer::Ok(reply)));
        let forge = GitHub { cli: cli.clone(), host: "github.com".into(), dir: None };
        let meta = forge.pr_meta(&key()).unwrap();
        assert_eq!(meta.users, ["Ana", "jo", "sam"]);
        assert_eq!(meta.labels, ["bug", "docs"]);
        assert_eq!(cli.calls().len(), 1);
    }

    #[test]
    fn states_reviews_and_merge_readiness() {
        assert_eq!(check_state("SUCCESS"), CheckState::Passing);
        assert_eq!(check_state("STARTUP_FAILURE"), CheckState::Failing);
        assert_eq!(check_state("QUEUED"), CheckState::Pending);
        assert_eq!(check_state("SKIPPED"), CheckState::None);
        let node = |state: &str, draft: bool, review: &str, mergeable: &str| {
            let j = format!(
                r#"{{"number":1,"title":"t","state":"{state}","isDraft":{draft},"url":"u","updatedAt":"2026-11-10T10:00:00Z","headRefName":"h","baseRefName":"main","author":null,"reviewDecision":"{review}","mergeable":"{mergeable}","mergeStateStatus":"CLEAN"}}"#
            );
            pr_from(&serde_json::from_str(&j).unwrap())
        };
        let draft = node("OPEN", true, "REVIEW_REQUIRED", "MERGEABLE");
        assert_eq!((draft.state, draft.review, draft.mergeable, draft.author.as_str()), (PrState::Draft, ReviewDecision::ReviewRequired, Mergeable::Clean, "ghost"));
        let blocked = node("OPEN", false, "CHANGES_REQUESTED", "CONFLICTING");
        assert_eq!((blocked.state, blocked.review, blocked.mergeable), (PrState::Open, ReviewDecision::ChangesRequested, Mergeable::Conflicts));
        assert_eq!(node("MERGED", false, "APPROVED", "UNKNOWN").state, PrState::Merged);
    }

    const DETAILS: &str = include_str!("fixtures/gh-pr-details.json");
    const CHECKS: &str = include_str!("fixtures/gh-pr-checks.json");
    const CONVERSATION: &str = include_str!("fixtures/gh-pr-conversation.json");
    const FAILED_LOG: &str = include_str!("fixtures/gh-run-log-failed.txt");

    fn gql<T: serde::de::DeserializeOwned>(json: &str) -> T {
        graphql_result(serde_json::from_str::<GraphQl<T>>(json).unwrap()).unwrap()
    }

    fn shop() -> RepoKey {
        RepoKey { host: "github.com".into(), path: "acme/shop".into() }
    }

    #[test]
    fn a_pull_requests_details_checks_and_conversation_are_read() {
        let d = parse_details(gql(DETAILS)).unwrap();
        assert_eq!((d.pr.number, d.pr.review, d.can_rerun), (13, ReviewDecision::ChangesRequested, true));
        assert!(d.body.starts_with("## Why"));

        let checks = parse_checks(gql(CHECKS)).unwrap();
        let by = |n: &str| checks.iter().find(|c| c.name == n).unwrap();
        assert_eq!(checks.len(), 6);
        assert_eq!((by("test (3.12)").state, &by("test (3.12)").kind), (CheckState::Failing, &CheckKind::Actions { run: 9001, job: Some(7002) }));
        assert_eq!(by("test (3.12)").duration(0), Some(125));
        assert_eq!((by("e2e").state, by("e2e").finished), (CheckState::Pending, None));
        assert_eq!((by("codecov/patch").state, &by("codecov/patch").kind), (CheckState::None, &CheckKind::Other));
        assert_eq!((by("ci/jenkins").state, by("ci/jenkins").url.as_deref()), (CheckState::Passing, Some("https://jenkins.acme.dev/job/shop/88")));
        let groups: Vec<CheckState> = group_checks(&checks).iter().map(|g| g.0).collect();
        assert_eq!(groups, [CheckState::Failing, CheckState::Pending, CheckState::Passing, CheckState::None]);

        let conv = parse_conversation(gql(CONVERSATION)).unwrap();
        let shape: Vec<(CommentKind, &str, Option<&str>, Option<u32>)> = conv.iter().map(|c| (c.kind, c.author.as_str(), c.path.as_deref(), c.line)).collect();
        assert_eq!(
            shape,
            [
                (CommentKind::Comment, "user-1", None, None),
                (CommentKind::Line, "user-2", Some("shop/cart.py"), Some(42)),
                (CommentKind::ChangesRequested, "user-2", None, None),
                (CommentKind::Line, "user-3", Some("shop/tax.py"), Some(7)),
                (CommentKind::Comment, "ghost", None, None),
            ],
            "oldest first; the empty review shows as its line comment; the pending one is left out"
        );
    }

    #[test]
    fn the_failed_log_is_the_failed_jobs_end() {
        let log = failed_job_log(FAILED_LOG, "test (3.12)");
        assert!(log.starts_with("── Set up job ──\nsetup 0\n"), "a heading per step, times dropped: {}", &log[..60]);
        assert!(!log.contains("another job"));
        let (end, cut) = trim_log(&log, LOG_LINES);
        let lines: Vec<&str> = end.lines().collect();
        assert_eq!(lines.len(), 300);
        assert!(cut > 0);
        assert_eq!(lines[298], "1 failed, 41 passed in 3.2s", "colours are gone");
        assert_eq!(lines[299], "##[error]Process completed with exit code 1.");
        // A job that isn't there: every job's lines, named.
        assert!(failed_job_log(FAILED_LOG, "nope").starts_with("── build › Compile ──\nerror from another job\n"));
        assert_eq!(failed_job_log("plain text\n", "x"), "plain text\n");

        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["run", "view", "9001", "--log-failed"], Answer::Ok(FAILED_LOG)));
        let gh = GitHub { cli: cli.clone(), host: "github.com".into(), dir: None };
        let checks = parse_checks(gql(CHECKS)).unwrap();
        let failed = checks.iter().find(|c| c.state == CheckState::Failing).unwrap();
        assert!(gh.check_log(&shop(), failed).unwrap().ends_with("exit code 1."));
        assert_eq!(cli.calls()[0].1, ["run", "view", "9001", "--log-failed", "-R", "github.com/acme/shop"]);
        let status = checks.iter().find(|c| c.name == "ci/jenkins").unwrap();
        assert_eq!(gh.check_log(&shop(), status), Err(ForgeError::NotFound));
    }

    #[test]
    fn rerun_runs_each_failed_workflow_run_once() {
        let failed = |name: &str, run: u64| Check { name: name.into(), state: CheckState::Failing, url: None, started: None, finished: None, kind: CheckKind::Actions { run, job: None } };
        let checks = vec![
            failed("test (3.12)", 9001),
            failed("test (3.11)", 9001),
            failed("deploy", 8000),
            Check { kind: CheckKind::Actions { run: 7000, job: None }, state: CheckState::Passing, ..failed("ok", 0) },
            Check { kind: CheckKind::Other, ..failed("ci/jenkins", 0) },
        ];
        let key = RepoKey { host: "github.acme.com".into(), path: "team/app".into() };
        let cmds = rerun_commands(&key, &checks);
        assert_eq!(cmds, [["run", "rerun", "8000", "--failed", "-R", "github.acme.com/team/app"], ["run", "rerun", "9001", "--failed", "-R", "github.acme.com/team/app"]]);

        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["run", "rerun"], Answer::Ok("")));
        let gh = GitHub { cli: cli.clone(), host: "github.acme.com".into(), dir: None };
        assert_eq!(gh.rerun_failed_checks(&key, 13, &checks), Ok(2));
        assert_eq!(cli.calls().len(), 2);
        let denied = GitHub { cli: Arc::new(StubCli::default().on(Tool::Gh, &["run", "rerun"], Answer::Fail(1, "HTTP 403: Must have admin rights to Repository."))), host: "github.com".into(), dir: None };
        assert!(matches!(denied.rerun_failed_checks(&key, 13, &checks), Err(ForgeError::Other(m)) if m.contains("403")));
    }

    /// Each details fixture, as the merge dialog shows what's in the way.
    #[test]
    fn merge_methods_and_blockers_come_with_the_details() {
        let d = parse_details(gql(DETAILS)).unwrap();
        let m = &d.merge;
        assert_eq!(m.methods, [MergeMethod::Merge, MergeMethod::Squash], "rebase isn't allowed here");
        assert_eq!((m.node_id.as_str(), m.status.as_str()), ("PR_kwDOAbc123", "BLOCKED"));
        assert!(m.can_merge && m.auto_merge_allowed && m.delete_branch && !m.auto_merge_on && !m.viewer_is_author);
        let words = |json: &str| -> Vec<String> {
            let d = parse_details(gql(json)).unwrap();
            blockers(ForgeKind::GitHub, &d.pr, &d.merge, None).iter().map(|b| b.text(ForgeKind::GitHub, &d.pr.base_branch)).collect()
        };
        assert_eq!(words(DETAILS), ["Changes were requested", "Required checks are failing"]);
        assert_eq!(words(include_str!("fixtures/gh-pr-details-clean.json")), Vec::<String>::new());
        assert_eq!(words(include_str!("fixtures/gh-pr-details-behind.json")), ["main has moved on: the branch must be brought up to date first"]);
        assert_eq!(words(include_str!("fixtures/gh-pr-details-dirty.json")), ["It has conflicts with main that must be resolved first"]);
        assert_eq!(words(include_str!("fixtures/gh-pr-details-draft.json")), ["It's a draft: mark it ready for review first"]);

        let clean = parse_details(gql(include_str!("fixtures/gh-pr-details-clean.json"))).unwrap();
        assert!(clean.merge.viewer_is_author && clean.merge.auto_merge_on);
        // Changes requested doesn't clear by itself, so auto-merge isn't offered.
        let b = blockers(ForgeKind::GitHub, &d.pr, &d.merge, None);
        assert_eq!(merge_ready(ForgeKind::GitHub, "main", &d.merge, &b), MergeReady::Blocked("Changes were requested".into()));
        let waiting = [Blocker::ChecksPending, Blocker::ReviewRequired];
        assert_eq!(merge_ready(ForgeKind::GitHub, "main", &d.merge, &waiting), MergeReady::Auto);
        let no_auto = MergeFacts { auto_merge_allowed: false, ..d.merge.clone() };
        assert_eq!(merge_ready(ForgeKind::GitHub, "main", &no_auto, &waiting), MergeReady::Blocked("Required checks are still running".into()));
        assert_eq!(merge_ready(ForgeKind::GitHub, "main", &d.merge, &[]), MergeReady::Now);
    }

    #[test]
    fn merging_reviewing_and_state_changes_are_gh_commands() {
        let key = RepoKey { host: "github.acme.com".into(), path: "team/app".into() };
        let squash = MergeRequest { method: MergeMethod::Squash, title: "Search (#12)".into(), message: "* Add search\n\n* Fix it".into(), delete_branch: true, auto: false, head: Some("abc123".into()) };
        assert_eq!(
            merge_args(&key, 12, &squash),
            ["pr", "merge", "12", "-R", "github.acme.com/team/app", "--squash", "--subject", "Search (#12)", "--body", "* Add search\n\n* Fix it", "--delete-branch", "--match-head-commit", "abc123"]
        );
        let auto = MergeRequest { method: MergeMethod::Rebase, delete_branch: false, auto: true, head: None, ..squash.clone() };
        assert_eq!(merge_args(&key, 12, &auto), ["pr", "merge", "12", "-R", "github.acme.com/team/app", "--rebase", "--auto"]);
        assert_eq!(state_args(&key, 3, StateChange::Draft), ["pr", "ready", "3", "--undo", "-R", "github.acme.com/team/app"]);
        assert_eq!(state_args(&key, 3, StateChange::Close), ["pr", "close", "3", "-R", "github.acme.com/team/app"]);
        assert_eq!(state_args(&key, 3, StateChange::Reopen), ["pr", "reopen", "3", "-R", "github.acme.com/team/app"]);
        assert_eq!(comment_args(&key, 3, "Nice"), ["pr", "comment", "3", "--body", "Nice", "-R", "github.acme.com/team/app"]);

        let line = |side, start, end| LineComment { path: "src/a \"b\".rs".into(), old_path: None, side, start, end, old_line: None, new_line: Some(end), body: "Why?\nReally".into() };
        let review = Review { verdict: Verdict::RequestChanges, body: "See notes".into(), comments: vec![line(Side::After, 3, 5), line(Side::Before, 9, 9)], pr_id: "PR_1".into() };
        assert_eq!(
            review_mutation(&review).unwrap(),
            "mutation { addPullRequestReview(input: { pullRequestId: \"PR_1\", event: REQUEST_CHANGES, body: \"See notes\", threads: [\
{ path: \"src/a \\\"b\\\".rs\", body: \"Why?\\nReally\", line: 5, side: RIGHT, startLine: 3, startSide: RIGHT }, \
{ path: \"src/a \\\"b\\\".rs\", body: \"Why?\\nReally\", line: 9, side: LEFT }] }) { pullRequestReview { url } } }"
        );

        // A plain comment is `gh pr comment`; a review is one GraphQL call. A refusal keeps gh's words.
        let cli = Arc::new(StubCli::default().on(Tool::Gh, &["pr", "comment"], Answer::Ok("")).on(Tool::Gh, &["api", "graphql"], Answer::Ok("{}")));
        let gh = GitHub { cli: cli.clone(), host: "github.acme.com".into(), dir: None };
        gh.review(&key, 3, &Review { verdict: Verdict::Comment, body: "Hi".into(), comments: vec![], pr_id: "PR_1".into() }).unwrap();
        gh.review(&key, 3, &Review { verdict: Verdict::Approve, body: String::new(), comments: vec![], pr_id: "PR_1".into() }).unwrap();
        let calls = cli.calls();
        assert_eq!(calls[0].1[..2], ["pr", "comment"]);
        assert!(calls[1].1[3].contains("event: APPROVE") && calls[1].1.ends_with(&["--hostname".to_string(), "github.acme.com".to_string()]));
        let refused = GitHub {
            cli: Arc::new(StubCli::default().on(Tool::Gh, &["pr", "merge"], Answer::Fail(1, "X Pull request team/app#12 is not mergeable: the base branch policy prohibits the merge.\nTo have the pull request merged after all the requirements have been met, add the `--auto` flag."))),
            host: "github.acme.com".into(),
            dir: None,
        };
        assert_eq!(
            refused.merge(&key, 12, &squash).unwrap_err().to_string(),
            "X Pull request team/app#12 is not mergeable: the base branch policy prohibits the merge.\nTo have the pull request merged after all the requirements have been met, add the `--auto` flag."
        );
    }

}
