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
//! CLI. `pulls` are [`PullRequest`]s as serde writes them; `users` and `labels`
//! are what a new pull request can name. `details` holds, by number, what a
//! pull request's view shows: its description, checks, conversation and the
//! logs of failed checks (by check name), and what merging needs (`merge`).
//! Checks run again turn pending and are listed in `reruns`; merges, reviews,
//! comments and state changes change the pull request and are listed in `done`. Pull requests created here are kept
//! until the app quits. Development builds use it when
//! `HEADWAY_FAKE_FORGE=<folder>` is set (and for the sample projects).

use super::*;
use std::sync::RwLock;

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
    pub users: Vec<String>,
    pub labels: Vec<String>,
    pub details: HashMap<u64, FakeDetails>,
    /// The checks run again (by name), in order.
    pub reruns: Vec<String>,
    /// What was done to pull requests here (merges, reviews, comments…), in
    /// order, in words: "merge #12 squash".
    pub done: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FakeDetails {
    pub body: String,
    pub can_rerun: bool,
    pub checks: Vec<Check>,
    pub conversation: Vec<PrComment>,
    /// Failed checks' logs, by check name.
    pub logs: HashMap<String, String>,
    /// What merging needs (the allowed methods default to the repository's;
    /// whether I'm the author is worked out).
    pub merge: MergeFacts,
}

pub struct FakeForges {
    data: Arc<RwLock<FakeData>>,
}

impl FakeForges {
    /// Read `<dir>/forge.json`; a missing or broken file is a forge that knows no host.
    pub fn load(dir: &Path) -> Self {
        let data = std::fs::read_to_string(dir.join("forge.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        FakeForges::new(data)
    }

    pub fn new(data: FakeData) -> Self {
        FakeForges { data: Arc::new(RwLock::new(data)) }
    }

    /// What the fixtures hold now (tests look at what was done).
    #[cfg(test)]
    pub fn data(&self) -> std::sync::RwLockReadGuard<'_, FakeData> {
        self.data.read().unwrap()
    }

    /// Change the fixtures, as if things happened on the forge (tests).
    #[cfg(test)]
    pub fn data_mut(&self) -> std::sync::RwLockWriteGuard<'_, FakeData> {
        self.data.write().unwrap()
    }
}

impl Forges for FakeForges {
    fn detect(&self, host: &str, _dir: Option<&Path>) -> HostKind {
        match self.data.read().unwrap().hosts.get(host) {
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
    pub data: Arc<RwLock<FakeData>>,
}

impl FakeForge {
    fn host(&self) -> ForgeResult<FakeHost> {
        let data = self.data.read().unwrap();
        let h = data.hosts.get(&self.host).ok_or(ForgeError::SignedOut { kind: self.kind, host: self.host.clone() })?;
        if !h.installed {
            return Err(ForgeError::NotInstalled(self.kind));
        }
        if h.user.is_none() {
            return Err(ForgeError::SignedOut { kind: self.kind, host: self.host.clone() });
        }
        Ok(h.clone())
    }

    /// Change pull request `number` (and its details) in place.
    fn change(&self, key: &RepoKey, number: u64, f: impl FnOnce(&mut FakeRepo, &mut PullRequest, &mut FakeDetails)) -> ForgeResult<()> {
        self.host()?;
        let mut data = self.data.write().unwrap();
        let r = data.repos.get_mut(&key.to_string()).ok_or(ForgeError::NotFound)?;
        let i = r.pulls.iter().position(|p| p.number == number).ok_or(ForgeError::NotFound)?;
        let mut pr = r.pulls[i].clone();
        let mut d = r.details.remove(&number).unwrap_or_default();
        f(r, &mut pr, &mut d);
        r.pulls[i] = pr;
        r.details.insert(number, d);
        Ok(())
    }

    fn repo(&self, key: &RepoKey) -> ForgeResult<FakeRepo> {
        self.host()?;
        self.data.read().unwrap().repos.get(&key.to_string()).cloned().ok_or(ForgeError::NotFound)
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

    /// Refused like the real ones: a second open pull request for the same branch.
    fn create_pr(&self, repo: &RepoKey, pr: &NewPr, me: &str) -> ForgeResult<PullRequest> {
        let r = self.repo(repo)?;
        let same = |p: &&PullRequest| p.head_branch == pr.head && p.head_repo == pr.head_repo && matches!(p.state, PrState::Open | PrState::Draft);
        if let Some(p) = r.pulls.iter().find(same) {
            return Err(ForgeError::Other(format!("a pull request for branch \"{}\" already exists: {}", pr.head, p.url)));
        }
        let number = r.pulls.iter().map(|p| p.number).max().unwrap_or(0) + 1;
        let web = r.info.as_ref().map_or_else(|| format!("https://{repo}"), |i| i.web_url.clone());
        let url = match self.kind {
            ForgeKind::GitHub => format!("{web}/pull/{number}"),
            ForgeKind::GitLab => format!("{web}/-/merge_requests/{number}"),
        };
        let created = created_pr(pr, me, url, number, now_secs());
        if let Some(r) = self.data.write().unwrap().repos.get_mut(&repo.to_string()) {
            r.pulls.push(created.clone());
        }
        Ok(created)
    }

    fn pr_meta(&self, repo: &RepoKey) -> ForgeResult<PrMeta> {
        let r = self.repo(repo)?;
        Ok(PrMeta { users: r.users, labels: r.labels })
    }

    fn pr_details(&self, repo: &RepoKey, number: u64) -> ForgeResult<PrDetails> {
        let r = self.repo(repo)?;
        let pr = r.pulls.iter().find(|p| p.number == number).cloned().ok_or(ForgeError::NotFound)?;
        let d = r.details.get(&number).cloned().unwrap_or_default();
        let me = self.host()?.user.unwrap_or_default();
        let mut merge = d.merge;
        if merge.methods.is_empty() {
            merge.methods = r.info.as_ref().map(|i| i.merge_methods.clone()).unwrap_or_default();
        }
        merge.viewer_is_author = pr.author == me;
        Ok(PrDetails { pr, body: d.body, can_rerun: d.can_rerun, merge })
    }

    /// Across the repositories asked about: open ones asking me for a review,
    /// mine open, and mine merged since `since`.
    fn inbox(&self, me: &str, repos: &[RepoKey], since: i64) -> ForgeResult<Inbox> {
        self.host()?;
        let data = self.data.read().unwrap();
        let mut inbox = Inbox::default();
        for key in repos {
            let Some(r) = data.repos.get(&key.to_string()) else { continue };
            for pr in &r.pulls {
                let item = || InboxPr { repo: key.clone(), pr: pr.clone() };
                let open = matches!(pr.state, PrState::Open | PrState::Draft);
                if open && pr.author != me && pr.requested_reviewers.iter().any(|r| r == me) {
                    inbox.review.push(item());
                }
                if open && pr.author == me {
                    inbox.mine.push(item());
                }
                if pr.state == PrState::Merged && pr.author == me && pr.updated >= since {
                    inbox.merged.push(item());
                }
            }
        }
        for list in [&mut inbox.review, &mut inbox.mine, &mut inbox.merged] {
            list.sort_by_key(|i| std::cmp::Reverse(i.pr.updated));
        }
        Ok(inbox.keep(repos))
    }

    /// Refused like the real ones when something's in the way (unless it's
    /// auto-merge, which waits); else it's merged.
    fn merge(&self, repo: &RepoKey, number: u64, req: &MergeRequest) -> ForgeResult<()> {
        let details = self.pr_details(repo, number)?;
        let checks = self.pr_checks(repo, number)?;
        let blocked = blockers(self.kind, &details.pr, &details.merge, Some(&checks));
        if let Some(b) = blocked.first().filter(|_| !req.auto) {
            return Err(ForgeError::Other(format!("Pull request {repo}#{number} is not mergeable: {}.", b.text(self.kind, &details.pr.base_branch).to_lowercase())));
        }
        if !details.merge.methods.contains(&req.method) {
            return Err(ForgeError::Other(format!("{} merges are not allowed on this repository.", req.method.verb())));
        }
        self.change(repo, number, |r, pr, d| {
            if req.auto {
                d.merge.auto_merge_on = true;
            } else {
                pr.state = PrState::Merged;
                pr.updated = now_secs();
            }
            let what = if req.auto { "auto-merge" } else { "merge" };
            let mut line = format!("{what} #{number} {:?}", req.method).to_lowercase();
            if req.method == MergeMethod::Squash {
                line.push_str(&format!(" “{}”", req.title));
            }
            if req.delete_branch {
                line.push_str(" delete-branch");
            }
            r.done.push(line);
        })
    }

    fn set_state(&self, repo: &RepoKey, number: u64, change: StateChange) -> ForgeResult<()> {
        self.change(repo, number, |r, pr, _| {
            pr.state = match change {
                StateChange::Ready | StateChange::Reopen => PrState::Open,
                StateChange::Draft => PrState::Draft,
                StateChange::Close => PrState::Closed,
            };
            r.done.push(format!("{} #{number}", change.label().to_lowercase()));
        })
    }

    fn comment(&self, repo: &RepoKey, number: u64, body: &str) -> ForgeResult<()> {
        let me = self.host()?.user.unwrap_or_default();
        self.change(repo, number, |r, _, d| {
            d.conversation.push(PrComment { kind: CommentKind::Comment, author: me.clone(), time: now_secs(), body: body.to_string(), path: None, line: None, url: None });
            r.done.push(format!("comment #{number}"));
        })
    }

    fn review(&self, repo: &RepoKey, number: u64, review: &Review) -> ForgeResult<()> {
        let me = self.host()?.user.unwrap_or_default();
        self.change(repo, number, |r, pr, d| {
            let now = now_secs();
            let kind = match review.verdict {
                Verdict::Approve => {
                    pr.review = ReviewDecision::Approved;
                    d.merge.viewer_approved = true;
                    CommentKind::Approved
                }
                Verdict::RequestChanges => {
                    pr.review = ReviewDecision::ChangesRequested;
                    CommentKind::ChangesRequested
                }
                Verdict::Unapprove => {
                    pr.review = ReviewDecision::ReviewRequired;
                    d.merge.viewer_approved = false;
                    CommentKind::Comment
                }
                Verdict::Comment => CommentKind::Reviewed,
            };
            d.conversation.push(PrComment { kind, author: me.clone(), time: now, body: review.body.clone(), path: None, line: None, url: None });
            for c in &review.comments {
                d.conversation.push(PrComment { kind: CommentKind::Line, author: me.clone(), time: now, body: c.body.clone(), path: Some(c.path.clone()), line: Some(c.end), url: None });
            }
            r.done.push(format!("review #{number} {:?} with {} line comments", review.verdict, review.comments.len()).to_lowercase());
        })
    }

    fn line_comment(&self, repo: &RepoKey, number: u64, c: &LineComment, _refs: &DiffRefs) -> ForgeResult<()> {
        let me = self.host()?.user.unwrap_or_default();
        self.change(repo, number, |r, _, d| {
            d.conversation.push(PrComment { kind: CommentKind::Line, author: me.clone(), time: now_secs(), body: c.body.clone(), path: Some(c.path.clone()), line: Some(c.end), url: None });
            r.done.push(format!("line comment #{number} {}", c.place()));
        })
    }

    fn pr_checks(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<Check>> {
        Ok(self.repo(repo)?.details.get(&number).map(|d| d.checks.clone()).unwrap_or_default())
    }

    fn conversation(&self, repo: &RepoKey, number: u64) -> ForgeResult<Vec<PrComment>> {
        Ok(in_time_order(self.repo(repo)?.details.get(&number).map(|d| d.conversation.clone()).unwrap_or_default()))
    }

    fn check_log(&self, repo: &RepoKey, check: &Check) -> ForgeResult<String> {
        let r = self.repo(repo)?;
        let log = r.details.values().find_map(|d| d.logs.get(&check.name)).ok_or(ForgeError::NotFound)?;
        Ok(trim_log(log, LOG_LINES).0)
    }

    fn rerun_failed_checks(&self, repo: &RepoKey, number: u64, _checks: &[Check]) -> ForgeResult<usize> {
        self.repo(repo)?;
        let mut data = self.data.write().unwrap();
        let r = data.repos.get_mut(&repo.to_string()).ok_or(ForgeError::NotFound)?;
        let d = r.details.get_mut(&number).ok_or(ForgeError::NotFound)?;
        if let Some(why) = rerun_blocked(self.kind, d.can_rerun, &d.checks) {
            return Err(ForgeError::Other(why));
        }
        let mut names = Vec::new();
        for c in d.checks.iter_mut().filter(|c| c.state == CheckState::Failing && c.has_log()) {
            c.state = CheckState::Pending;
            c.finished = None;
            names.push(c.name.clone());
        }
        let n = names.len();
        r.reruns.extend(names);
        Ok(n)
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

        // Creating one: numbered after the others, and there from then on.
        let new = NewPr { head: "fix/cart".into(), base: "main".into(), title: "Fix cart".into(), ..Default::default() };
        let pr = gh.create_pr(&key, &new, "alex").unwrap();
        assert_eq!((pr.number, pr.url.as_str(), pr.author.as_str()), (4, "https://github.com/acme/shop/pull/4", "alex"));
        assert_eq!(forges.forge(ForgeKind::GitHub, "github.com", None).prs_for_branches(&key, &["fix/cart".into()]).unwrap().by_branch["fix/cart"][0].number, 4);
        assert!(gh.create_pr(&key, &new, "alex").unwrap_err().to_string().contains("already exists"));
        assert_eq!(gh.pr_meta(&key).unwrap(), PrMeta::default());
    }

    /// Merging, reviewing and closing change the pull request, as on the forge;
    /// a merge with something in the way is refused unless it's auto-merge.
    #[test]
    fn pull_requests_are_merged_reviewed_and_closed() {
        let data = crate::samples::sample_forge(1_000_000);
        let forges = FakeForges::new(data);
        let gh = forges.forge(ForgeKind::GitHub, "github.com", None);
        let key = RepoKey { host: "github.com".into(), path: "headway-samples/web-shop".into() };
        let squash = |auto| MergeRequest { method: MergeMethod::Squash, title: "Fix (#13)".into(), message: String::new(), delete_branch: true, auto, head: None };
        let refused = gh.merge(&key, 13, &squash(false)).unwrap_err().to_string();
        assert_eq!(refused, "Pull request github.com/headway-samples/web-shop#13 is not mergeable: changes were requested.");
        gh.merge(&key, 13, &squash(true)).unwrap();
        assert!(gh.pr_details(&key, 13).unwrap().merge.auto_merge_on);

        gh.review(&key, 14, &Review { verdict: Verdict::Approve, body: "Nice".into(), comments: vec![], pr_id: "x".into() }).unwrap();
        assert_eq!(gh.pr_details(&key, 14).unwrap().pr.review, ReviewDecision::Approved);
        assert_eq!(gh.conversation(&key, 14).unwrap().last().map(|c| (c.kind, c.author.as_str())), Some((CommentKind::Approved, "alex-rivera")));

        gh.merge(&key, 12, &squash(false)).unwrap();
        assert_eq!(gh.pr_details(&key, 12).unwrap().pr.state, PrState::Merged);
        gh.set_state(&key, 14, StateChange::Close).unwrap();
        assert_eq!(gh.pr_details(&key, 14).unwrap().pr.state, PrState::Closed);
        let done = forges.data.read().unwrap().repos["github.com/headway-samples/web-shop"].done.clone();
        assert_eq!(done, ["auto-merge #13 squash “Fix (#13)” delete-branch", "review #14 approve with 0 line comments", "merge #12 squash “Fix (#13)” delete-branch", "close #14"]);
    }

}
