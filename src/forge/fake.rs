//! A forge made of JSON files, for the sample projects, screenshots and tests.
//!
//! A fixture folder holds `forge.json`:
//!
//! ```json
//! {
//!   "hosts": { "github.com": { "kind": "github", "user": "alex" } },
//!   "repos": { "github.com/headway-samples/web-shop": { "info": { … }, "pulls": [ … ] } }
//! }
//! ```
//!
//! A host with `"user": null` is signed out, one with `"installed": false` has no
//! CLI. `pulls` are [`PullRequest`]s as serde writes them. Development builds use
//! it when `HEADWAY_FAKE_FORGE=<folder>` is set (and for the sample projects).

use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FakeData {
    pub hosts: HashMap<String, FakeHost>,
    /// Keyed by `host/path`.
    pub repos: HashMap<String, FakeRepo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FakeHost {
    pub kind: ForgeKind,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "yes")]
    pub installed: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FakeRepo {
    pub info: Option<RepoInfo>,
    pub pulls: Vec<PullRequest>,
}

pub struct FakeForges {
    data: Arc<FakeData>,
}

impl FakeForges {
    /// Read `<dir>/forge.json`; a missing or broken file is a forge that knows no host.
    pub fn load(dir: &Path) -> Self {
        let data = std::fs::read_to_string(dir.join("forge.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        FakeForges { data: Arc::new(data) }
    }

    pub fn new(data: FakeData) -> Self {
        FakeForges { data: Arc::new(data) }
    }
}

impl Forges for FakeForges {
    fn detect(&self, host: &str, _dir: Option<&Path>) -> HostKind {
        match self.data.hosts.get(host) {
            Some(h) => HostKind { kind: Some(h.kind), signed_in: h.installed && h.user.is_some() },
            None => HostKind { kind: None, signed_in: false },
        }
    }

    fn forge(&self, kind: ForgeKind, host: &str, _dir: Option<&Path>) -> Arc<dyn Forge> {
        Arc::new(FakeForge { kind, host: host.to_string(), data: self.data.clone() })
    }
}

pub struct FakeForge {
    pub kind: ForgeKind,
    pub host: String,
    pub data: Arc<FakeData>,
}

impl FakeForge {
    fn host(&self) -> ForgeResult<&FakeHost> {
        let h = self.data.hosts.get(&self.host).ok_or(ForgeError::SignedOut { kind: self.kind, host: self.host.clone() })?;
        if !h.installed {
            return Err(ForgeError::NotInstalled(self.kind));
        }
        if h.user.is_none() {
            return Err(ForgeError::SignedOut { kind: self.kind, host: self.host.clone() });
        }
        Ok(h)
    }

    fn repo(&self, key: &RepoKey) -> ForgeResult<&FakeRepo> {
        self.host()?;
        self.data.repos.get(&key.to_string()).ok_or(ForgeError::NotFound)
    }
}

impl Forge for FakeForge {
    fn kind(&self) -> ForgeKind {
        self.kind
    }

    fn whoami(&self) -> ForgeResult<String> {
        Ok(self.host()?.user.clone().unwrap_or_default())
    }

    fn repo_info(&self, repo: &RepoKey) -> ForgeResult<RepoInfo> {
        self.repo(repo)?.info.clone().ok_or(ForgeError::NotFound)
    }

    fn prs_for_branches(&self, repo: &RepoKey, branches: &[String]) -> ForgeResult<BranchPrs> {
        let r = self.repo(repo)?;
        let by_branch = branches
            .iter()
            .map(|b| {
                let mut prs: Vec<PullRequest> = r.pulls.iter().filter(|p| &p.head_branch == b).cloned().collect();
                prs.sort_by_key(|p| std::cmp::Reverse(p.updated));
                (b.clone(), prs)
            })
            .collect();
        Ok(BranchPrs { by_branch, slow_down_until: None })
    }

    fn pr_lists(&self, repo: &RepoKey, me: &str, limit: usize) -> ForgeResult<PrLists> {
        let r = self.repo(repo)?;
        let mut open: Vec<PullRequest> = r.pulls.iter().filter(|p| matches!(p.state, PrState::Open | PrState::Draft)).cloned().collect();
        open.sort_by_key(|p| std::cmp::Reverse(p.updated));
        let pick = |f: &dyn Fn(&PullRequest) -> bool| open.iter().filter(|p| f(p)).take(limit).cloned().collect();
        Ok(PrLists {
            mine: pick(&|p| p.author == me),
            review_requested: pick(&|p| p.requested_reviewers.iter().any(|r| r == me)),
            open_total: open.len(),
            open: open.iter().take(limit).cloned().collect(),
            slow_down_until: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_answer_like_a_forge() {
        let json = r#"{
            "hosts": {"github.com": {"kind": "github", "user": "alex"}, "gitlab.com": {"kind": "gitlab", "user": null}, "git.corp": {"kind": "gitlab", "installed": false}},
            "repos": {"github.com/acme/shop": {"info": {"default_branch": "main", "merge_methods": ["squash"], "web_url": "https://github.com/acme/shop"},
              "pulls": [{"number": 3, "title": "Search", "state": "open", "author": "sam", "head_branch": "feature/search", "base_branch": "main", "url": "u", "updated": 10}]}}
        }"#;
        let forges = FakeForges::new(serde_json::from_str(json).unwrap());
        assert_eq!(forges.detect("github.com", None), HostKind { kind: Some(ForgeKind::GitHub), signed_in: true });
        assert_eq!(forges.detect("example.com", None).kind, None);
        let gh = forges.forge(ForgeKind::GitHub, "github.com", None);
        assert_eq!(gh.whoami().unwrap(), "alex");
        let key = RepoKey { host: "github.com".into(), path: "acme/shop".into() };
        assert_eq!(gh.repo_info(&key).unwrap().merge_methods, [MergeMethod::Squash]);
        let prs = gh.prs_for_branches(&key, &["feature/search".into(), "main".into()]).unwrap();
        assert_eq!(prs.by_branch["feature/search"][0].number, 3);
        assert_eq!(prs.by_branch["feature/search"][0].checks.state, CheckState::None, "left-out fields take their defaults");
        assert!(prs.by_branch["main"].is_empty());
        let lists = gh.pr_lists(&key, "sam", 50).unwrap();
        assert_eq!((lists.mine.len(), lists.review_requested.len(), lists.open_total), (1, 0, 1));
        assert_eq!(gh.prs_for_branches(&RepoKey { host: "github.com".into(), path: "x/y".into() }, &[]).unwrap_err(), ForgeError::NotFound);
        assert_eq!(forges.forge(ForgeKind::GitLab, "gitlab.com", None).whoami().unwrap_err(), ForgeError::SignedOut { kind: ForgeKind::GitLab, host: "gitlab.com".into() });
        assert_eq!(forges.forge(ForgeKind::GitLab, "git.corp", None).whoami().unwrap_err(), ForgeError::NotInstalled(ForgeKind::GitLab));
    }
}
