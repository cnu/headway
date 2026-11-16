//! GitLab through `glab api`: GraphQL for merge requests and pipelines (one call
//! for any number of branches), REST for the project's settings.

use super::*;
use serde::Deserialize;

pub struct GitLab {
    pub cli: Arc<dyn Cli>,
    pub host: String,
    pub dir: Option<PathBuf>,
}

impl GitLab {
    /// Run a glab command that changes something, keeping a refusal's own words.
    fn act(&self, args: &[String]) -> ForgeResult<String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run_action(self.cli.as_ref(), ForgeKind::GitLab, &self.host, self.dir.as_deref(), &args)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> ForgeResult<T> {
        let mut all: Vec<&str> = args.to_vec();
        all.extend(["--hostname", self.host.as_str()]);
        run_json(self.cli.as_ref(), ForgeKind::GitLab, &self.host, self.dir.as_deref(), &all)
    }
}

impl Forge for GitLab {
    fn kind(&self) -> ForgeKind {
        ForgeKind::GitLab
    }

    fn whoami(&self) -> ForgeResult<String> {
        #[derive(Deserialize)]
        struct User {
            username: String,
        }
        Ok(self.json::<User>(&["api", "user"])?.username)
    }

    fn repo_info(&self, repo: &RepoKey) -> ForgeResult<RepoInfo> {
        let endpoint = format!("projects/{}", encode_path(&repo.path));
        Ok(parse_project(self.json(&["api", &endpoint])?))
    }

    fn prs_for_branches(&self, repo: &RepoKey, branches: &[String]) -> ForgeResult<BranchPrs> {
        if branches.is_empty() {
            return Ok(BranchPrs::default());
        }
        let q = format!("query={}", mrs_query(branches));
        let path = format!("path={}", repo.path);
        let reply: GraphQl = self.json(&["api", "graphql", "-f", &q, "-f", &path])?;
        parse_mrs(reply, branches)
    }

    fn pr_lists(&self, repo: &RepoKey, me: &str, limit: usize) -> ForgeResult<PrLists> {
        let q = format!("query={}", lists_query(limit));
        let path = format!("path={}", repo.path);
        let me = format!("me={me}");
        let reply: ListsReply = self.json(&["api", "graphql", "-f", &q, "-f", &path, "-f", &me])?;
        parse_lists(reply)
    }

    fn create_pr(&self, repo: &RepoKey, pr: &NewPr, me: &str) -> ForgeResult<PullRequest> {
        let args = create_args(repo, pr);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_write(self.cli.as_ref(), ForgeKind::GitLab, &self.host, self.dir.as_deref(), &args)?;
        let (url, number) = created_url(&out, ForgeKind::GitLab).ok_or_else(|| ForgeError::Other("glab didn't say which merge request it opened".into()))?;
        Ok(created_pr(pr, me, url, number, now_secs()))
    }

    fn pr_meta(&self, repo: &RepoKey) -> ForgeResult<PrMeta> {
        #[derive(Deserialize)]
        struct Member {
            username: String,
        }
        #[derive(Deserialize)]
        struct Label {
            name: String,
        }
        let project = encode_path(&repo.path);
        let members: Vec<Member> = self.json(&["api", &format!("projects/{project}/members/all?per_page=100")])?;
        let labels: Vec<Label> = self.json(&["api", &format!("projects/{project}/labels?per_page=100")])?;
        let mut users: Vec<String> = members.into_iter().map(|m| m.username).collect();
        users.sort_by_key(|u| u.to_lowercase());
        users.dedup();
        let mut labels: Vec<String> = labels.into_iter().map(|l| l.name).collect();
        labels.sort_by_key(|l| l.to_lowercase());
        Ok(PrMeta { users, labels })
    }

    fn pr_details(&self, repo: &RepoKey, number: u64) -> ForgeResult<PrDetails> {
        let q = format!("query={}", details_query(number));
        let path = format!("path={}", repo.path);
        let reply = self.json(&["api", "graphql", "-f", &q, "-f", &path])?;
        // How the project merges is in its settings, which GraphQL doesn't show.
        let project: Option<Project> = self.json(&["api", &format!("projects/{}", encode_path(&repo.path))]).ok();
        parse_details(reply, project.as_ref())
    }

    fn pr_checks(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<Check>> {
        let q = format!("query={}", checks_query(number));
        let path = format!("path={}", repo.path);
        parse_checks(self.json(&["api", "graphql", "-f", &q, "-f", &path])?, &self.host)
    }

    fn conversation(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<PrComment>> {
        let q = format!("query={}", conversation_query(number));
        let path = format!("path={}", repo.path);
        parse_conversation(self.json(&["api", "graphql", "-f", &q, "-f", &path])?)
    }

    /// The job's trace, through the REST API.
    fn check_log(&self, repo: &RepoKey, check: &Check) -> ForgeResult<String> {
        let CheckKind::GitLabJob { job, .. } = check.kind else { return Err(ForgeError::NotFound) };
        let endpoint = format!("projects/{}/jobs/{job}/trace", encode_path(&repo.path));
        let text = run_text(self.cli.as_ref(), ForgeKind::GitLab, &self.host, self.dir.as_deref(), &["api", &endpoint, "--hostname", &self.host])?;
        Ok(trim_log(&text, LOG_LINES).0)
    }

    /// One GraphQL call for the signed-in user's three lists, kept to `repos`.
    fn inbox(&self, _me: &str, repos: &[RepoKey], since: i64) -> ForgeResult<Inbox> {
        let q = format!("query={}", inbox_query(since));
        let reply: InboxReply = self.json(&["api", "graphql", "-f", &q])?;
        Ok(parse_inbox(reply, &self.host)?.keep(repos))
    }

    fn merge(&self, repo: &RepoKey, number: u64, req: &MergeRequest) -> ForgeResult<()> {
        self.act(&merge_args(&self.host, repo, number, req)).map(drop)
    }

    fn set_state(&self, repo: &RepoKey, number: u64, change: StateChange) -> ForgeResult<()> {
        self.act(&state_args(&self.host, repo, number, change)).map(drop)
    }

    fn comment(&self, repo: &RepoKey, number: u64, body: &str) -> ForgeResult<()> {
        self.act(&note_args(&self.host, repo, number, body)).map(drop)
    }

    /// Approve or take back an approval, then the comment (if any) as a note.
    fn review(&self, repo: &RepoKey, number: u64, review: &Review) -> ForgeResult<()> {
        let call = match review.verdict {
            Verdict::Approve => Some("approve"),
            Verdict::Unapprove => Some("unapprove"),
            Verdict::Comment => None,
            Verdict::RequestChanges => return Err(ForgeError::NotYet("request changes on GitLab")),
        };
        if let Some(call) = call {
            let endpoint = format!("{}/{call}", mr_endpoint(repo, number));
            self.act(&["api".into(), "-X".into(), "POST".into(), endpoint, "--hostname".into(), self.host.clone()])?;
        }
        if !review.body.trim().is_empty() {
            self.comment(repo, number, &review.body)?;
        }
        Ok(())
    }

    fn line_comment(&self, repo: &RepoKey, number: u64, comment: &LineComment, refs: &DiffRefs) -> ForgeResult<()> {
        self.act(&discussion_args(&self.host, repo, number, comment, refs)).map(drop)
    }

    /// Retry the pipeline (GitLab runs its failed jobs again).
    fn rerun_failed_checks(&self, repo: &RepoKey, _number: u64, checks: &[Check]) -> ForgeResult<usize> {
        let all = rerun_commands(&self.host, repo, checks);
        for args in &all {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run_write(self.cli.as_ref(), ForgeKind::GitLab, &self.host, self.dir.as_deref(), &args)?;
        }
        Ok(all.len())
    }
}

/// One `glab api -X POST …/pipelines/<id>/retry` per pipeline with a failed job
/// (which retries that pipeline's failed jobs).
pub fn rerun_commands(host: &str, repo: &RepoKey, checks: &[Check]) -> Vec<Vec<String>> {
    let mut pipelines: Vec<u64> = checks
        .iter()
        .filter(|c| c.state == CheckState::Failing)
        .filter_map(|c| match c.kind {
            CheckKind::GitLabJob { pipeline, .. } => Some(pipeline),
            _ => None,
        })
        .collect();
    pipelines.sort_unstable();
    pipelines.dedup();
    let project = encode_path(&repo.path);
    pipelines
        .into_iter()
        .map(|p| ["api", "-X", "POST", &format!("projects/{project}/pipelines/{p}/retry"), "--hostname", host].map(String::from).to_vec())
        .collect()
}

/// `projects/<id>/merge_requests/<iid>` in the REST API.
fn mr_endpoint(repo: &RepoKey, number: u64) -> String {
    format!("projects/{}/merge_requests/{number}", encode_path(&repo.path))
}

/// `glab api … --hostname <host>`, with `fields` (`-f` for text, `-F` for
/// true and false, which glab sends as booleans).
fn api_call(host: &str, method: &str, endpoint: String, fields: &[(&str, String)]) -> Vec<String> {
    let mut a: Vec<String> = vec!["api".into(), "-X".into(), method.into(), endpoint];
    for (k, v) in fields {
        let flag = if v == "true" || v == "false" { "-F" } else { "-f" };
        a.extend([flag.into(), format!("{k}={v}")]);
    }
    a.extend(["--hostname".into(), host.into()]);
    a
}

/// The REST merge call: GitLab merges the way the project is set to; squashing
/// (with its message) and deleting the source branch are asked for here. The
/// head commit is pinned, so a push since isn't merged unseen.
pub fn merge_args(host: &str, repo: &RepoKey, number: u64, req: &MergeRequest) -> Vec<String> {
    let mut fields: Vec<(&str, String)> = Vec::new();
    if req.method == MergeMethod::Squash {
        fields.push(("squash", "true".into()));
        let msg = [req.title.trim(), req.message.trim()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("\n\n");
        if !msg.is_empty() {
            fields.push(("squash_commit_message", msg));
        }
    }
    if req.delete_branch {
        fields.push(("should_remove_source_branch", "true".into()));
    }
    if let Some(h) = &req.head {
        fields.push(("sha", h.clone()));
    }
    api_call(host, "PUT", format!("{}/merge", mr_endpoint(repo, number)), &fields)
}

/// Ready and draft through `glab mr update`; closing and reopening through
/// the REST API's state event.
pub fn state_args(host: &str, repo: &RepoKey, number: u64, change: StateChange) -> Vec<String> {
    match change {
        StateChange::Ready | StateChange::Draft => {
            let flag = if change == StateChange::Ready { "--ready" } else { "--draft" };
            vec!["mr".into(), "update".into(), number.to_string(), flag.into(), format!("--repo=https://{host}/{}", repo.path)]
        }
        StateChange::Close | StateChange::Reopen => {
            let event = if change == StateChange::Close { "close" } else { "reopen" };
            api_call(host, "PUT", mr_endpoint(repo, number), &[("state_event", event.into())])
        }
    }
}

pub fn note_args(host: &str, repo: &RepoKey, number: u64, body: &str) -> Vec<String> {
    api_call(host, "POST", format!("{}/notes", mr_endpoint(repo, number)), &[("body", body.to_string())])
}

/// A discussion on the diff. Its position goes in the address (as
/// `position[…]=` parameters, which GitLab reads into the nested position); an
/// unchanged line is placed by both its numbers. A range is placed on its last
/// line.
pub fn discussion_args(host: &str, repo: &RepoKey, number: u64, c: &LineComment, refs: &DiffRefs) -> Vec<String> {
    let old_path = c.old_path.as_deref().unwrap_or(&c.path);
    let mut pos: Vec<(&str, String)> = vec![
        ("position_type", "text".into()),
        ("base_sha", refs.base.clone()),
        ("start_sha", refs.start.clone()),
        ("head_sha", refs.head.clone()),
        ("old_path", old_path.to_string()),
        ("new_path", c.path.clone()),
    ];
    if let Some(n) = c.old_line {
        pos.push(("old_line", n.to_string()));
    }
    if let Some(n) = c.new_line {
        pos.push(("new_line", n.to_string()));
    }
    let query: Vec<String> = pos.iter().map(|(k, v)| format!("position%5B{k}%5D={}", encode_path(v))).collect();
    let endpoint = format!("{}/discussions?{}", mr_endpoint(repo, number), query.join("&"));
    api_call(host, "POST", endpoint, &[("body", c.body.clone())])
}

/// `glab mr create` for `pr`, never asking anything. Values are written
/// `--flag=value`; lists are comma-separated, as glab takes them.
pub fn create_args(repo: &RepoKey, pr: &NewPr) -> Vec<String> {
    let mut a: Vec<String> = ["mr", "create", "--yes"].map(String::from).to_vec();
    a.push(format!("--repo=https://{}/{}", repo.host, repo.path));
    if let Some(fork) = &pr.head_repo {
        a.push(format!("--head={fork}"));
    }
    a.push(format!("--source-branch={}", pr.head));
    a.push(format!("--target-branch={}", pr.base));
    a.push(format!("--title={}", pr.title));
    a.push(format!("--description={}", pr.body));
    for (flag, on) in [("--draft", pr.draft), ("--remove-source-branch", pr.remove_source_branch), ("--squash-before-merge", pr.squash)] {
        if on {
            a.push(flag.into());
        }
    }
    for (flag, names) in [("reviewer", &pr.reviewers), ("assignee", &pr.assignees), ("label", &pr.labels)] {
        if !names.is_empty() {
            a.push(format!("--{flag}={}", names.join(",")));
        }
    }
    a
}

/// A project path for the REST API: `group/sub/repo` → `group%2Fsub%2Frepo`.
pub fn encode_path(path: &str) -> String {
    path.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Merge requests from any of `branches`, newest first, with their pipeline's jobs.
/// Branch names are written as JSON strings, which GraphQL reads the same way.
pub fn mrs_query(branches: &[String]) -> String {
    let names: Vec<String> = branches.iter().map(|b| serde_json::to_string(b).unwrap_or_default()).collect();
    format!(
        "query($path: ID!) {{ project(fullPath: $path) {{ mergeRequests(sourceBranches: [{}], first: 100, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} }} }} {MR_FRAGMENT}",
        names.join(", ")
    )
}

/// Everything the app reads about a merge request.
const MR_FRAGMENT: &str = "fragment mr on MergeRequest { \
iid title state draft webUrl updatedAt sourceBranch targetBranch diffHeadSha author { username } sourceProject { fullPath } targetProject { fullPath } \
approved detailedMergeStatus reviewers { nodes { username } } headPipeline { status jobs(first: 100) { nodes { status allowFailure } } } }";

/// The inbox: merge requests asking for my review, mine open, and mine merged
/// since `since`, across every project (the caller keeps the workspace's).
pub fn inbox_query(since: i64) -> String {
    format!(
        "query {{ currentUser {{ \
review: reviewRequestedMergeRequests(state: opened, first: 100, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} \
mine: authoredMergeRequests(state: opened, first: 100, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} \
merged: authoredMergeRequests(state: merged, mergedAfter: \"{}T00:00:00Z\", first: 50, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} }} }} {MR_FRAGMENT}",
        day_of(since)
    )
}

/// One query for the sidebar's three lists (by me, asking me for a review, all
/// open), `limit` each, newest first.
pub fn lists_query(limit: usize) -> String {
    let n = limit.clamp(1, 100);
    format!(
        "query($path: ID!, $me: String!) {{ project(fullPath: $path) {{ \
mine: mergeRequests(state: opened, authorUsername: $me, first: {n}, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} \
review: mergeRequests(state: opened, reviewerUsername: $me, first: {n}, sort: UPDATED_DESC) {{ nodes {{ ...mr }} }} \
open: mergeRequests(state: opened, first: {n}, sort: UPDATED_DESC) {{ count nodes {{ ...mr }} }} }} }} {MR_FRAGMENT}"
    )
}

// ---- Parsing (public to the tests, which feed fixtures)

#[derive(Deserialize)]
pub struct Project {
    default_branch: Option<String>,
    web_url: String,
    #[serde(default)]
    merge_method: String,
    #[serde(default)]
    squash_option: String,
    #[serde(default)]
    remove_source_branch_after_merge: bool,
}

pub fn parse_project(p: Project) -> RepoInfo {
    let merge_methods = merge_methods(&p);
    RepoInfo { default_branch: p.default_branch.unwrap_or_default(), merge_methods, web_url: p.web_url }
}

/// How a project's merge requests can be merged: its one merge method
/// (fast-forward, or a merge commit — "rebase_merge" makes one too, once the
/// branch is rebased), and squashing unless the project never allows it.
/// A project that always squashes offers only that.
pub fn merge_methods(p: &Project) -> Vec<MergeMethod> {
    let plain = match p.merge_method.as_str() {
        "ff" => MergeMethod::FastForward,
        _ => MergeMethod::Merge,
    };
    match p.squash_option.as_str() {
        "always" => vec![MergeMethod::Squash],
        "never" => vec![plain],
        _ => vec![plain, MergeMethod::Squash],
    }
}

#[derive(Deserialize)]
pub struct GraphQl {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct Data {
    project: Option<ProjectNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectNode {
    merge_requests: Nodes<MrNode>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrNode {
    iid: String,
    title: String,
    state: String,
    #[serde(default)]
    draft: bool,
    web_url: String,
    updated_at: String,
    source_branch: String,
    target_branch: String,
    #[serde(default)]
    diff_head_sha: Option<String>,
    author: Option<Username>,
    source_project: Option<FullPath>,
    target_project: Option<FullPath>,
    #[serde(default)]
    approved: bool,
    detailed_merge_status: Option<String>,
    reviewers: Option<Nodes<Username>>,
    head_pipeline: Option<Pipeline>,
}

#[derive(Deserialize)]
struct Username {
    username: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullPath {
    full_path: String,
}

#[derive(Deserialize)]
struct Pipeline {
    status: String,
    jobs: Option<Nodes<Job>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    status: String,
    #[serde(default)]
    allow_failure: bool,
}

/// A job's (or pipeline's) status, in four groups. A job allowed to fail doesn't fail.
pub fn job_state(status: &str, allow_failure: bool) -> CheckState {
    match status.to_ascii_uppercase().as_str() {
        "SUCCESS" => CheckState::Passing,
        "FAILED" if allow_failure => CheckState::None,
        "FAILED" => CheckState::Failing,
        "CREATED" | "WAITING_FOR_RESOURCE" | "PREPARING" | "PENDING" | "RUNNING" | "SCHEDULED" | "WAITING_FOR_CALLBACK" | "CANCELING" => CheckState::Pending,
        _ => CheckState::None,
    }
}

fn gql_error(errors: &[GqlError]) -> ForgeResult<()> {
    match errors.first() {
        Some(e) if e.message.to_ascii_lowercase().contains("rate limit") => Err(ForgeError::RateLimited { until: None }),
        Some(e) => Err(ForgeError::Other(e.message.clone())),
        None => Ok(()),
    }
}

#[derive(Deserialize)]
pub struct ListsReply {
    data: Option<ListsData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct ListsData {
    project: Option<ListsProject>,
}

#[derive(Deserialize)]
struct ListsProject {
    mine: Option<Nodes<MrNode>>,
    review: Option<Nodes<MrNode>>,
    open: Option<CountedNodes>,
}

#[derive(Deserialize)]
struct CountedNodes {
    #[serde(default)]
    count: usize,
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<MrNode>>,
}

pub fn parse_lists(reply: ListsReply) -> ForgeResult<PrLists> {
    gql_error(&reply.errors)?;
    let p = reply.data.and_then(|d| d.project).ok_or(ForgeError::NotFound)?;
    let read = |n: Option<Nodes<MrNode>>| n.map(|n| n.nodes.iter().flatten().map(mr_from).collect()).unwrap_or_default();
    let (open, open_total) = p.open.map(|o| (o.nodes.iter().flatten().map(mr_from).collect(), o.count)).unwrap_or_default();
    Ok(PrLists { mine: read(p.mine), review_requested: read(p.review), open, open_total, slow_down_until: None })
}

pub fn parse_mrs(reply: GraphQl, branches: &[String]) -> ForgeResult<BranchPrs> {
    gql_error(&reply.errors)?;
    let project = reply.data.and_then(|d| d.project).ok_or(ForgeError::NotFound)?;
    let mut by_branch: HashMap<String, Vec<PullRequest>> = branches.iter().map(|b| (b.clone(), Vec::new())).collect();
    for n in project.merge_requests.nodes.iter().flatten() {
        if let Some(list) = by_branch.get_mut(&n.source_branch) {
            list.push(mr_from(n));
        }
    }
    for list in by_branch.values_mut() {
        list.sort_by_key(|p| std::cmp::Reverse(p.updated));
    }
    Ok(BranchPrs { by_branch, slow_down_until: None })
}

fn mr_from(n: &MrNode) -> PullRequest {
    let state = match n.state.as_str() {
        "merged" => PrState::Merged,
        "closed" | "locked" => PrState::Closed,
        _ if n.draft => PrState::Draft,
        _ => PrState::Open,
    };
    let status = n.detailed_merge_status.as_deref().unwrap_or("");
    let review = match status {
        "REQUESTED_CHANGES" => ReviewDecision::ChangesRequested,
        _ if n.approved && status != "NOT_APPROVED" => ReviewDecision::Approved,
        "NOT_APPROVED" | "APPROVALS_SYNCING" => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    };
    let mergeable = match status {
        "MERGEABLE" => Mergeable::Clean,
        "CONFLICT" | "NEED_REBASE" => Mergeable::Conflicts,
        "CHECKING" | "UNCHECKED" | "PREPARING" | "" => Mergeable::Unknown,
        _ => Mergeable::Blocked,
    };
    let checks = match &n.head_pipeline {
        Some(p) => match p.jobs.as_ref().filter(|j| !j.nodes.is_empty()) {
            Some(jobs) => CheckSummary::from_states(jobs.nodes.iter().flatten().map(|j| (job_state(&j.status, j.allow_failure), 1))),
            None => CheckSummary::from_states([(job_state(&p.status, false), 1)]),
        },
        None => CheckSummary::default(),
    };
    let source = n.source_project.as_ref().map(|p| p.full_path.clone());
    let target = n.target_project.as_ref().map(|p| p.full_path.clone());
    PullRequest {
        number: n.iid.parse().unwrap_or(0),
        title: n.title.clone(),
        state,
        author: n.author.as_ref().map_or_else(|| "ghost".to_string(), |a| a.username.clone()),
        head_branch: n.source_branch.clone(),
        base_branch: n.target_branch.clone(),
        head_repo: source.filter(|s| target.as_ref() != Some(s)),
        url: n.web_url.clone(),
        review,
        requested_reviewers: n.reviewers.iter().flat_map(|r| r.nodes.iter().flatten()).map(|u| u.username.clone()).collect(),
        checks,
        mergeable,
        updated: parse_time(&n.updated_at).unwrap_or(0),
        head_oid: n.diff_head_sha.clone(),
    }
}

/// The merge request with its description, and whether the user may retry its pipeline.
pub fn details_query(number: u64) -> String {
    format!(
        "query($path: ID!) {{ currentUser {{ username }} project(fullPath: $path) {{ mergeRequest(iid: \"{number}\") {{ ...mr description \
rerun: headPipeline {{ userPermissions {{ updatePipeline }} }} userPermissions {{ canMerge }} approvedBy {{ nodes {{ username }} }} \
diffRefs {{ baseSha startSha headSha }} autoMergeEnabled }} }} }} {MR_FRAGMENT}"
    )
}

/// The jobs of the merge request's latest pipeline (not the ones retried since).
pub fn checks_query(number: u64) -> String {
    format!(
        "query($path: ID!) {{ project(fullPath: $path) {{ mergeRequest(iid: \"{number}\") {{ headPipeline {{ id jobs(first: 100, retried: false) {{ nodes {{ id name status allowFailure webPath startedAt finishedAt }} }} }} }} }} }}"
    )
}

/// Every note, in its threads; with the file and line for notes on the diff.
pub fn conversation_query(number: u64) -> String {
    format!(
        "query($path: ID!) {{ project(fullPath: $path) {{ mergeRequest(iid: \"{number}\") {{ discussions(first: 100) {{ nodes {{ notes {{ nodes {{ \
author {{ username }} body createdAt system url position {{ newPath newLine oldPath oldLine }} }} }} }} }} }} }} }}"
    )
}

#[derive(Deserialize)]
pub struct InboxReply {
    data: Option<InboxData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InboxData {
    current_user: Option<InboxUser>,
}

#[derive(Deserialize)]
struct InboxUser {
    review: Option<Nodes<MrNode>>,
    mine: Option<Nodes<MrNode>>,
    merged: Option<Nodes<MrNode>>,
}

/// Each merge request with the project it's in (the one it merges into).
pub fn parse_inbox(reply: InboxReply, host: &str) -> ForgeResult<Inbox> {
    gql_error(&reply.errors)?;
    let user = reply.data.and_then(|d| d.current_user).ok_or(ForgeError::NotFound)?;
    let list = |n: Option<Nodes<MrNode>>| -> Vec<InboxPr> {
        n.map(|n| n.nodes).unwrap_or_default().iter().flatten().filter_map(|m| Some(InboxPr { repo: RepoKey { host: host.to_string(), path: m.target_project.as_ref()?.full_path.clone() }, pr: mr_from(m) })).collect()
    };
    Ok(Inbox { review: list(user.review), mine: list(user.mine), merged: list(user.merged), ..Default::default() })
}

#[derive(Deserialize)]
pub struct Reply<T> {
    data: Option<ProjectOf<T>>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectOf<T> {
    project: Option<MrOf<T>>,
    /// Asked for with the details only.
    #[serde(default)]
    current_user: Option<Username>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrOf<T> {
    merge_request: Option<T>,
}

fn merge_request<T>(reply: Reply<T>) -> ForgeResult<T> {
    gql_error(&reply.errors)?;
    reply.data.and_then(|d| d.project).and_then(|p| p.merge_request).ok_or(ForgeError::NotFound)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailsMr {
    #[serde(flatten)]
    mr: MrNode,
    #[serde(default)]
    description: Option<String>,
    rerun: Option<RerunPipeline>,
    user_permissions: Option<MrPermissions>,
    approved_by: Option<Nodes<Username>>,
    diff_refs: Option<GqlDiffRefs>,
    #[serde(default)]
    auto_merge_enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MrPermissions {
    #[serde(default)]
    can_merge: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlDiffRefs {
    base_sha: Option<String>,
    start_sha: Option<String>,
    head_sha: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RerunPipeline {
    user_permissions: Option<PipelinePermissions>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PipelinePermissions {
    #[serde(default)]
    update_pipeline: bool,
}

/// The merge request's details, with the project's settings (`project`, from
/// the REST API: how it merges and whether it deletes source branches).
pub fn parse_details(reply: Reply<DetailsMr>, project: Option<&Project>) -> ForgeResult<PrDetails> {
    let me = reply.data.as_ref().and_then(|d| d.current_user.as_ref()).map(|u| u.username.clone());
    let d = merge_request(reply)?;
    let can_rerun = d.rerun.and_then(|r| r.user_permissions).is_some_and(|p| p.update_pipeline);
    let pr = mr_from(&d.mr);
    let mine = |name: &str| me.as_deref() == Some(name);
    let merge = MergeFacts {
        node_id: String::new(),
        status: d.mr.detailed_merge_status.clone().unwrap_or_default(),
        viewer_is_author: mine(&pr.author),
        viewer_approved: d.approved_by.as_ref().is_some_and(|n| n.nodes.iter().flatten().any(|u| mine(&u.username))),
        can_merge: d.user_permissions.is_some_and(|p| p.can_merge),
        methods: project.map(merge_methods).unwrap_or_default(),
        auto_merge_allowed: false,
        auto_merge_on: d.auto_merge_enabled,
        delete_branch: project.is_some_and(|p| p.remove_source_branch_after_merge),
        diff_refs: d.diff_refs.and_then(|r| Some(DiffRefs { base: r.base_sha?, start: r.start_sha?, head: r.head_sha? })),
    };
    Ok(PrDetails { pr, body: d.description.unwrap_or_default(), can_rerun, merge })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksMr {
    head_pipeline: Option<ChecksPipeline>,
}

#[derive(Deserialize)]
struct ChecksPipeline {
    id: String,
    jobs: Option<Nodes<JobNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobNode {
    id: Option<String>,
    name: String,
    status: String,
    #[serde(default)]
    allow_failure: bool,
    web_path: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

/// The number at the end of a global id ("gid://gitlab/Ci::Build/123" → 123).
pub fn gid_number(gid: &str) -> Option<u64> {
    gid.rsplit('/').next()?.parse().ok()
}

pub fn parse_checks(reply: Reply<ChecksMr>, host: &str) -> ForgeResult<Vec<Check>> {
    let Some(p) = merge_request(reply)?.head_pipeline else { return Ok(Vec::new()) };
    let pipeline = gid_number(&p.id);
    let time = |t: &Option<String>| t.as_deref().and_then(parse_time);
    Ok(p.jobs
        .map(|j| j.nodes)
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .map(|j| {
            let kind = match (j.id.as_deref().and_then(gid_number), pipeline) {
                (Some(job), Some(pipeline)) => CheckKind::GitLabJob { job, pipeline },
                _ => CheckKind::Other,
            };
            Check {
                state: job_state(&j.status, j.allow_failure),
                url: j.web_path.as_ref().map(|w| format!("https://{host}{w}")),
                started: time(&j.started_at),
                finished: time(&j.finished_at),
                name: j.name,
                kind,
            }
        })
        .collect())
}

#[derive(Deserialize)]
pub struct ConversationMr {
    discussions: Nodes<Discussion>,
}

#[derive(Deserialize)]
struct Discussion {
    notes: Nodes<Note>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    author: Option<Username>,
    #[serde(default)]
    body: String,
    created_at: String,
    #[serde(default)]
    system: bool,
    url: Option<String>,
    position: Option<Position>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Position {
    new_path: Option<String>,
    new_line: Option<u32>,
    old_path: Option<String>,
    old_line: Option<u32>,
}

/// Notes oldest first. GitLab's own notes ("added 2 commits") are left out,
/// except approvals and requests for changes, which read like reviews.
pub fn parse_conversation(reply: Reply<ConversationMr>) -> ForgeResult<Vec<PrComment>> {
    let mr = merge_request(reply)?;
    let mut out = Vec::new();
    for n in mr.discussions.nodes.into_iter().flatten().flat_map(|d| d.notes.nodes).flatten() {
        let kind = match (n.system, &n.position) {
            (true, _) if n.body.starts_with("approved this merge request") => CommentKind::Approved,
            (true, _) if n.body.starts_with("requested changes") => CommentKind::ChangesRequested,
            (true, _) => continue,
            (false, Some(_)) => CommentKind::Line,
            (false, None) => CommentKind::Comment,
        };
        let (path, line) = match n.position {
            Some(p) => (p.new_path.or(p.old_path), p.new_line.or(p.old_line)),
            None => (None, None),
        };
        let body = if n.system { String::new() } else { n.body };
        out.push(PrComment {
            kind,
            author: n.author.map_or_else(|| "ghost".to_string(), |a| a.username),
            time: parse_time(&n.created_at).unwrap_or(0),
            body,
            path,
            line,
            url: n.url,
        });
    }
    Ok(in_time_order(out))
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Answer, StubCli};
    use super::*;

    fn forge(cli: Arc<StubCli>) -> GitLab {
        GitLab { cli, host: "gitlab.example.org".into(), dir: None }
    }

    #[test]
    fn merge_requests_and_pipelines_are_read() {
        let reply: GraphQl = serde_json::from_str(include_str!("fixtures/glab-mrs-for-branches.json")).unwrap();
        let branches: Vec<String> = ["feature/search", "wip/dark-mode", "fix/rounding", "old/merged", "nothing-here"].iter().map(|s| s.to_string()).collect();
        let prs = parse_mrs(reply, &branches).unwrap();
        assert_eq!(prs.by_branch.len(), 5);
        assert!(prs.by_branch["nothing-here"].is_empty());

        let open = &prs.by_branch["feature/search"][0];
        assert_eq!((open.number, open.state, open.review, open.mergeable), (41, PrState::Open, ReviewDecision::Approved, Mergeable::Clean));
        assert_eq!(open.checks, CheckSummary { passing: 3, none: 1, state: CheckState::Passing, ..Default::default() });
        assert_eq!(open.requested_reviewers, ["reviewer-1"]);
        assert_eq!(open.head_repo, None);

        let draft = &prs.by_branch["wip/dark-mode"][0];
        assert_eq!((draft.state, draft.checks.state, draft.head_repo.as_deref()), (PrState::Draft, CheckState::Pending, Some("contributor-2/shop")));

        let failing = &prs.by_branch["fix/rounding"][0];
        assert_eq!((failing.review, failing.checks.failing, failing.checks.state), (ReviewDecision::ChangesRequested, 1, CheckState::Failing));

        let merged = &prs.by_branch["old/merged"];
        assert_eq!(merged.iter().map(|p| (p.number, p.state)).collect::<Vec<_>>(), [(38, PrState::Merged), (12, PrState::Closed)], "newest first");
        assert_eq!(merged[0].updated, parse_time("2026-11-03T17:45:00.000Z").unwrap());
        assert_eq!(merged[0].head_oid.as_deref(), Some("3f2a9c1d0e8b7a6f5e4d3c2b1a0f9e8d7c6b5a49"));
        assert_eq!(open.head_oid, None);
    }

    #[test]
    fn one_call_for_all_branches_with_names_quoted() {
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &["api", "graphql"], Answer::Ok(r#"{"data":{"project":{"mergeRequests":{"nodes":[]}}}}"#)));
        let f = forge(cli.clone());
        let key = RepoKey { host: "gitlab.example.org".into(), path: "group/sub/shop".into() };
        let prs = f.prs_for_branches(&key, &["a\"b".into(), "c\\d".into(), "e".into()]).unwrap();
        assert_eq!(prs.by_branch.len(), 3);
        let calls = cli.calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0].1;
        assert!(args.contains(&"path=group/sub/shop".to_string()));
        assert!(args.windows(2).any(|w| w == ["--hostname", "gitlab.example.org"]));
        let q = args.iter().find(|a| a.starts_with("query=")).unwrap();
        assert!(q.contains(r#"sourceBranches: ["a\"b", "c\\d", "e"]"#), "{q}");
        assert!(f.prs_for_branches(&key, &[]).unwrap().by_branch.is_empty());
        assert_eq!(cli.calls().len(), 1, "no branches, no call");
    }

    #[test]
    fn lists_are_read_in_one_call() {
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &["api", "graphql"], Answer::Ok(include_str!("fixtures/glab-mr-lists.json"))));
        let f = forge(cli.clone());
        let lists = f.pr_lists(&RepoKey { host: "gitlab.example.org".into(), path: "group/sub/shop".into() }, "tanuki-user", 50).unwrap();
        assert_eq!((lists.mine.len(), lists.review_requested.len(), lists.open.len(), lists.open_total), (1, 1, 4, 4));
        assert_eq!((lists.mine[0].number, lists.mine[0].author.as_str()), (44, "tanuki-user"));
        assert_eq!(lists.review_requested[0].requested_reviewers, ["tanuki-user"]);
        assert!(lists.open.iter().any(|p| p.state == PrState::Draft && p.head_repo.is_some()), "a draft from a fork");
        let args = &cli.calls()[0].1;
        assert!(args.contains(&"me=tanuki-user".to_string()) && args.contains(&"path=group/sub/shop".to_string()));
        let q = args.iter().find(|a| a.starts_with("query=")).unwrap();
        assert!(q.contains("authorUsername: $me") && q.contains("reviewerUsername: $me") && q.contains("first: 50") && q.contains("count"), "{q}");
    }

    #[test]
    fn project_settings_and_user_are_read() {
        let cli = Arc::new(
            StubCli::default()
                .on(Tool::Glab, &["api", "projects/group%2Fsub%2Fshop"], Answer::Ok(include_str!("fixtures/glab-project.json")))
                .on(Tool::Glab, &["api", "user"], Answer::Ok(include_str!("fixtures/glab-user.json"))),
        );
        let f = forge(cli);
        let info = f.repo_info(&RepoKey { host: "gitlab.example.org".into(), path: "group/sub/shop".into() }).unwrap();
        assert_eq!(info.default_branch, "main");
        assert_eq!(info.merge_methods, [MergeMethod::FastForward, MergeMethod::Squash]);
        assert_eq!(info.web_url, "https://gitlab.example.org/group/sub/shop");
        assert_eq!(f.whoami().unwrap(), "tanuki-user");
        assert_eq!(encode_path("a b/c.d"), "a%20b%2Fc.d");
    }

    #[test]
    fn creating_a_merge_request_with_gitlab_options() {
        let new = NewPr {
            head: "fix/rounding".into(),
            head_repo: Some("contributor-2/shop".into()),
            base: "main".into(),
            title: "Round totals".into(),
            body: "Closes #3".into(),
            reviewers: vec!["reviewer-1".into(), "reviewer-2".into()],
            labels: vec!["bug".into()],
            remove_source_branch: true,
            squash: true,
            ..Default::default()
        };
        let printed = "\nCreating merge request for fix/rounding into main in group/sub/shop\n\n!57 Round totals (fix/rounding)\n https://gitlab.example.org/group/sub/shop/-/merge_requests/57\n";
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &["mr", "create"], Answer::Ok(printed)));
        let pr = forge(cli.clone()).create_pr(&RepoKey { host: "gitlab.example.org".into(), path: "group/sub/shop".into() }, &new, "tanuki-user").unwrap();
        assert_eq!((pr.number, pr.state, pr.author.as_str()), (57, PrState::Open, "tanuki-user"));
        let args = &cli.calls()[0].1;
        for want in [
            "--yes",
            "--repo=https://gitlab.example.org/group/sub/shop",
            "--head=contributor-2/shop",
            "--source-branch=fix/rounding",
            "--target-branch=main",
            "--description=Closes #3",
            "--remove-source-branch",
            "--squash-before-merge",
            "--reviewer=reviewer-1,reviewer-2",
            "--label=bug",
        ] {
            assert!(args.contains(&want.to_string()), "{want} in {args:?}");
        }
        assert!(!args.iter().any(|a| a == "--draft" || a.starts_with("--assignee")));
    }

    #[test]
    fn members_and_labels_are_listed() {
        let cli = Arc::new(
            StubCli::default()
                .on(Tool::Glab, &["api", "projects/group%2Fshop/members/all?per_page=100"], Answer::Ok(r#"[{"username":"zed"},{"username":"amy"}]"#))
                .on(Tool::Glab, &["api", "projects/group%2Fshop/labels?per_page=100"], Answer::Ok(r#"[{"name":"UX"},{"name":"bug"}]"#)),
        );
        let meta = forge(cli).pr_meta(&RepoKey { host: "gitlab.example.org".into(), path: "group/shop".into() }).unwrap();
        assert_eq!((meta.users, meta.labels), (vec!["amy".to_string(), "zed".into()], vec!["bug".to_string(), "UX".into()]));
    }

    #[test]
    fn errors_and_limits() {
        let reply: GraphQl = serde_json::from_str(r#"{"data":{"project":null}}"#).unwrap();
        assert_eq!(parse_mrs(reply, &["x".into()]).unwrap_err(), ForgeError::NotFound);
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &[], Answer::Fail(1, "glab: 429 Too Many Requests (HTTP 429)")));
        assert_eq!(forge(cli).prs_for_branches(&RepoKey { host: "h".into(), path: "a/b".into() }, &["x".into()]).unwrap_err(), ForgeError::RateLimited { until: None });
        assert_eq!(job_state("failed", true), CheckState::None);
        assert_eq!(job_state("MANUAL", false), CheckState::None);
        assert_eq!(job_state("RUNNING", false), CheckState::Pending);
    }

    const MR_DETAILS: &str = include_str!("fixtures/glab-mr-details.json");
    const MR_CHECKS: &str = include_str!("fixtures/glab-mr-checks.json");
    const MR_CONVERSATION: &str = include_str!("fixtures/glab-mr-conversation.json");

    fn shop() -> RepoKey {
        RepoKey { host: "gitlab.com".into(), path: "acme/shop".into() }
    }

    #[test]
    fn a_merge_requests_details_jobs_and_notes_are_read() {
        let d = parse_details(serde_json::from_str(MR_DETAILS).unwrap(), None).unwrap();
        assert_eq!((d.pr.number, d.pr.state, d.can_rerun), (13, PrState::Draft, false));
        assert!(d.body.starts_with("Fixes rounding."));

        let checks = parse_checks(serde_json::from_str(MR_CHECKS).unwrap(), "gitlab.com").unwrap();
        let shape: Vec<(&str, CheckState)> = checks.iter().map(|c| (c.name.as_str(), c.state)).collect();
        assert_eq!(
            shape,
            [("lint", CheckState::Passing), ("test", CheckState::Failing), ("flaky-e2e", CheckState::None), ("deploy-review", CheckState::None), ("package", CheckState::Pending)],
            "a job allowed to fail doesn't fail"
        );
        assert_eq!(checks[1].kind, CheckKind::GitLabJob { job: 8802, pipeline: 5501 });
        assert_eq!(checks[1].url.as_deref(), Some("https://gitlab.com/acme/shop/-/jobs/8802"));
        assert_eq!(checks[1].duration(0), Some(180));

        let conv = parse_conversation(serde_json::from_str(MR_CONVERSATION).unwrap()).unwrap();
        let shape: Vec<(CommentKind, &str, Option<u32>)> = conv.iter().map(|c| (c.kind, c.author.as_str(), c.line)).collect();
        assert_eq!(
            shape,
            [(CommentKind::Comment, "tanuki-user", None), (CommentKind::Line, "reviewer-1", Some(42)), (CommentKind::Line, "tanuki-user", Some(40)), (CommentKind::Approved, "reviewer-1", None)],
            "oldest first, GitLab's own notes left out but approvals kept"
        );
        assert_eq!(conv[1].path.as_deref(), Some("shop/cart.py"));
        assert_eq!(gid_number("gid://gitlab/Ci::Build/123"), Some(123));
        assert_eq!(gid_number("nope"), None);
    }

    #[test]
    fn the_failed_jobs_trace_and_pipeline_retry() {
        let long: String = (1..=400).map(|i| format!("\x1b[0Ksection {i}\n")).collect();
        let leaked: &'static str = Box::leak(long.into_boxed_str());
        let cli = Arc::new(
            StubCli::default()
                .on(Tool::Glab, &["api", "projects/acme%2Fshop/jobs/8802/trace"], Answer::Ok(leaked))
                .on(Tool::Glab, &["api", "-X", "POST"], Answer::Ok("{}")),
        );
        let gl = forge(cli.clone());
        let checks = parse_checks(serde_json::from_str(MR_CHECKS).unwrap(), "gitlab.com").unwrap();
        let log = gl.check_log(&shop(), &checks[1]).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!((lines.len(), lines[0], lines[299]), (300, "section 101", "section 400"));
        assert_eq!(gl.check_log(&shop(), &Check { kind: CheckKind::Other, ..checks[1].clone() }), Err(ForgeError::NotFound));

        let key = RepoKey { host: "git.corp".into(), path: "group/sub/app".into() };
        let two = vec![checks[1].clone(), Check { kind: CheckKind::GitLabJob { job: 1, pipeline: 5501 }, ..checks[1].clone() }, checks[2].clone()];
        assert_eq!(rerun_commands("git.corp", &key, &two), [["api", "-X", "POST", "projects/group%2Fsub%2Fapp/pipelines/5501/retry", "--hostname", "git.corp"]], "one retry per pipeline");
        assert!(rerun_commands("git.corp", &key, &checks[..1]).is_empty(), "nothing failed");
        assert_eq!(gl.rerun_failed_checks(&shop(), 13, &checks), Ok(1));
        let calls = cli.calls();
        assert_eq!(calls.last().unwrap().1, ["api", "-X", "POST", "projects/acme%2Fshop/pipelines/5501/retry", "--hostname", "gitlab.example.org"]);
    }

    /// The project's settings decide the methods; each details fixture shows
    /// what's in the way.
    #[test]
    fn merge_methods_and_blockers_come_with_the_details() {
        let project: Project = serde_json::from_str(include_str!("fixtures/glab-project.json")).unwrap();
        let methods = |merge: &str, squash: &str| merge_methods(&Project { merge_method: merge.into(), squash_option: squash.into(), ..serde_json::from_str(include_str!("fixtures/glab-project.json")).unwrap() });
        assert_eq!(methods("merge", "default_off"), [MergeMethod::Merge, MergeMethod::Squash]);
        assert_eq!(methods("rebase_merge", "never"), [MergeMethod::Merge]);
        assert_eq!(methods("ff", "default_on"), [MergeMethod::FastForward, MergeMethod::Squash]);
        assert_eq!(methods("merge", "always"), [MergeMethod::Squash], "a project that always squashes");

        let d = parse_details(serde_json::from_str(MR_DETAILS).unwrap(), Some(&project)).unwrap();
        let m = &d.merge;
        assert_eq!(m.methods, [MergeMethod::FastForward, MergeMethod::Squash]);
        assert!(m.can_merge && m.viewer_approved && !m.viewer_is_author && !m.auto_merge_allowed);
        assert_eq!(m.diff_refs.as_ref().map(|r| r.start.as_str()), Some("2222222222222222222222222222222222222222"));
        let words = |json: &str| -> Vec<String> {
            let d = parse_details(serde_json::from_str(json).unwrap(), Some(&project)).unwrap();
            blockers(ForgeKind::GitLab, &d.pr, &d.merge, None).iter().map(|b| b.text(ForgeKind::GitLab, &d.pr.base_branch)).collect()
        };
        assert_eq!(words(MR_DETAILS), ["It's a draft: mark it ready for review first"]);
        assert_eq!(words(include_str!("fixtures/glab-mr-details-rebase.json")), ["main has moved on: the branch must be brought up to date first"]);
        assert_eq!(
            words(include_str!("fixtures/glab-mr-details-ci.json")),
            ["You don't have permission to merge into main", "Required checks are failing"]
        );
    }

    #[test]
    fn merging_reviewing_and_state_changes_are_glab_commands() {
        let key = RepoKey { host: "git.corp".into(), path: "group/app".into() };
        let squash = MergeRequest { method: MergeMethod::Squash, title: "Search".into(), message: "* Add search".into(), delete_branch: true, auto: false, head: Some("abc".into()) };
        assert_eq!(
            merge_args("git.corp", &key, 7, &squash),
            [
                "api", "-X", "PUT", "projects/group%2Fapp/merge_requests/7/merge", "-F", "squash=true", "-f", "squash_commit_message=Search\n\n* Add search", "-F", "should_remove_source_branch=true", "-f", "sha=abc", "--hostname",
                "git.corp"
            ]
        );
        let plain = MergeRequest { method: MergeMethod::FastForward, delete_branch: false, head: None, ..squash.clone() };
        assert_eq!(merge_args("git.corp", &key, 7, &plain), ["api", "-X", "PUT", "projects/group%2Fapp/merge_requests/7/merge", "--hostname", "git.corp"]);
        assert_eq!(state_args("git.corp", &key, 7, StateChange::Ready), ["mr", "update", "7", "--ready", "--repo=https://git.corp/group/app"]);
        assert_eq!(state_args("git.corp", &key, 7, StateChange::Close), ["api", "-X", "PUT", "projects/group%2Fapp/merge_requests/7", "-f", "state_event=close", "--hostname", "git.corp"]);
        assert_eq!(note_args("git.corp", &key, 7, "Nice"), ["api", "-X", "POST", "projects/group%2Fapp/merge_requests/7/notes", "-f", "body=Nice", "--hostname", "git.corp"]);

        let refs = DiffRefs { base: "b".into(), start: "s".into(), head: "h".into() };
        let unchanged = LineComment { path: "src/new name.rs".into(), old_path: Some("src/old.rs".into()), side: Side::After, start: 3, end: 4, old_line: Some(2), new_line: Some(4), body: "Hm".into() };
        let a = discussion_args("git.corp", &key, 7, &unchanged, &refs);
        assert_eq!(
            a[3],
            "projects/group%2Fapp/merge_requests/7/discussions?position%5Bposition_type%5D=text&position%5Bbase_sha%5D=b&position%5Bstart_sha%5D=s&position%5Bhead_sha%5D=h\
&position%5Bold_path%5D=src%2Fold.rs&position%5Bnew_path%5D=src%2Fnew%20name.rs&position%5Bold_line%5D=2&position%5Bnew_line%5D=4"
        );
        assert_eq!(a[4..], ["-f", "body=Hm", "--hostname", "git.corp"]);

        // Approving is its own call, then the comment as a note; a refusal keeps GitLab's words.
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &["api", "-X", "POST"], Answer::Ok("{}")));
        let gl = forge(cli.clone());
        gl.review(&shop(), 13, &Review { verdict: Verdict::Approve, body: "LGTM".into(), comments: vec![], pr_id: String::new() }).unwrap();
        let calls: Vec<String> = cli.calls().iter().map(|c| c.1[3].clone()).collect();
        assert_eq!(calls, ["projects/acme%2Fshop/merge_requests/13/approve", "projects/acme%2Fshop/merge_requests/13/notes"]);
        gl.review(&shop(), 13, &Review { verdict: Verdict::Unapprove, body: String::new(), comments: vec![], pr_id: String::new() }).unwrap();
        assert_eq!(cli.calls().last().unwrap().1[3], "projects/acme%2Fshop/merge_requests/13/unapprove");
        let refused = forge(Arc::new(StubCli::default().on(Tool::Glab, &["api", "-X", "PUT"], Answer::Fail(1, "glab: 405 Method Not Allowed (HTTP 405)\n{\"message\":\"405 Method Not Allowed\"}"))));
        assert_eq!(refused.merge(&shop(), 13, &plain).unwrap_err().to_string(), "glab: 405 Method Not Allowed (HTTP 405)\n{\"message\":\"405 Method Not Allowed\"}");
    }


    #[test]
    fn the_inbox_reads_my_lists_and_groups_each_merge_request() {
        use crate::inbox::{Group, InboxPrefs, group};
        let repos = [RepoKey { host: "gitlab.example.org".into(), path: "group/app".into() }];
        let inbox = parse_inbox(serde_json::from_str(include_str!("fixtures/glab-inbox.json")).unwrap(), "gitlab.example.org").unwrap().keep(&repos);
        assert_eq!((inbox.review.len(), inbox.mine.len(), inbox.merged.len()), (1, 2, 1), "group/elsewhere is left out");
        let g = group(&[&inbox], &InboxPrefs::default(), 0);
        let shape: Vec<(Group, Vec<u64>)> = g.groups.iter().map(|(g, l)| (*g, l.iter().map(|i| i.pr.pr.number).collect())).collect();
        assert_eq!(shape, [(Group::NeedsReview, vec![31]), (Group::Failing, vec![13]), (Group::Ready, vec![14])]);
        let q = inbox_query(parse_time("2026-11-16T15:00:00Z").unwrap());
        assert!(q.contains("merged: authoredMergeRequests(state: merged, mergedAfter: \"2026-11-16T00:00:00Z\""));
        // Through glab: one call.
        let cli = Arc::new(StubCli::default().on(Tool::Glab, &["api", "graphql"], Answer::Ok(include_str!("fixtures/glab-inbox.json"))));
        let gl = forge(cli.clone());
        let got = gl.inbox("tanuki-user", &[RepoKey { host: "gitlab.example.org".into(), path: "group/app".into() }], 0).unwrap();
        assert_eq!((got.review.len(), cli.calls().len()), (1, 1));
    }

}
