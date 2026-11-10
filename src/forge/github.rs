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
}

/// One query for `n` branches: an aliased `pullRequests(headRefName:)` per branch
/// (names go in as variables, never into the query text), plus the rate limit.
pub fn prs_query(n: usize) -> String {
    let vars: String = (0..n).map(|i| format!(", $h{i}: String!")).collect();
    let fields: String = (0..n)
        .map(|i| format!(" b{i}: pullRequests(headRefName: $h{i}, first: 5, orderBy: {{field: UPDATED_AT, direction: DESC}}) {{ nodes {{ ...pr }} }}"))
        .collect();
    format!(
        "query($owner: String!, $name: String!{vars}) {{ rateLimit {{ remaining resetAt }} repository(owner: $owner, name: $name) {{{fields} }} }} \
fragment pr on PullRequest {{ number title state isDraft url updatedAt headRefName headRefOid baseRefName isCrossRepository \
author {{ login }} headRepository {{ nameWithOwner }} reviewDecision mergeable mergeStateStatus \
reviewRequests(first: 20) {{ nodes {{ requestedReviewer {{ ... on User {{ login }} ... on Team {{ slug }} ... on Bot {{ login }} }} }} }} \
commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts {{ checkRunCountsByState {{ state count }} statusContextCountsByState {{ state count }} }} }} }} }} }} }}"
    )
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
}
