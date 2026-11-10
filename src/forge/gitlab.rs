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
        "query($path: ID!) {{ project(fullPath: $path) {{ mergeRequests(sourceBranches: [{}], first: 100, sort: UPDATED_DESC) {{ nodes {{ \
iid title state draft webUrl updatedAt sourceBranch targetBranch author {{ username }} sourceProject {{ fullPath }} targetProject {{ fullPath }} \
approved detailedMergeStatus reviewers {{ nodes {{ username }} }} headPipeline {{ status jobs(first: 100) {{ nodes {{ status allowFailure }} }} }} }} }} }} }}",
        names.join(", ")
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
}

pub fn parse_project(p: Project) -> RepoInfo {
    // "ff" and "rebase_merge" both need the branch rebased first.
    let mut merge_methods = vec![match p.merge_method.as_str() {
        "ff" => MergeMethod::Rebase,
        _ => MergeMethod::Merge,
    }];
    if p.squash_option != "never" {
        merge_methods.push(MergeMethod::Squash);
    }
    RepoInfo { default_branch: p.default_branch.unwrap_or_default(), merge_methods, web_url: p.web_url }
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

pub fn parse_mrs(reply: GraphQl, branches: &[String]) -> ForgeResult<BranchPrs> {
    if let Some(e) = reply.errors.first() {
        return Err(if e.message.to_ascii_lowercase().contains("rate limit") { ForgeError::RateLimited { until: None } } else { ForgeError::Other(e.message.clone()) });
    }
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
    }
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
    fn project_settings_and_user_are_read() {
        let cli = Arc::new(
            StubCli::default()
                .on(Tool::Glab, &["api", "projects/group%2Fsub%2Fshop"], Answer::Ok(include_str!("fixtures/glab-project.json")))
                .on(Tool::Glab, &["api", "user"], Answer::Ok(include_str!("fixtures/glab-user.json"))),
        );
        let f = forge(cli);
        let info = f.repo_info(&RepoKey { host: "gitlab.example.org".into(), path: "group/sub/shop".into() }).unwrap();
        assert_eq!(info.default_branch, "main");
        assert_eq!(info.merge_methods, [MergeMethod::Rebase, MergeMethod::Squash]);
        assert_eq!(info.web_url, "https://gitlab.example.org/group/sub/shop");
        assert_eq!(f.whoami().unwrap(), "tanuki-user");
        assert_eq!(encode_path("a b/c.d"), "a%20b%2Fc.d");
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
}
