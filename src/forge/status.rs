//! Which pull request belongs to which local branch, and how to put it in words.
//!
//! A branch is matched by its upstream (the remote and the branch name there),
//! not by its local name, so a local `fix` tracking `origin/fix-login` finds the
//! pull request from `fix-login`. When the upstream is a fork, the pull request's
//! head project must be that fork too.

use super::*;

/// A local branch, as far as pull requests are concerned.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalBranch {
    pub name: String,
    pub upstream_remote: Option<String>,
    pub upstream_branch: Option<String>,
    pub current: bool,
    /// The commit it's at, when known (to tell whether a merged branch moved on).
    pub tip: Option<String>,
}

impl LocalBranch {
    pub fn from_git(b: &git::Branch) -> Self {
        let tip = (!b.tip.is_empty()).then(|| b.tip.clone());
        LocalBranch { name: b.name.clone(), upstream_remote: b.upstream_remote.clone(), upstream_branch: b.upstream_branch.clone(), current: b.is_head, tip }
    }
}

/// Where a branch's pull request would come from: the branch name on the forge,
/// and the fork's project path (`None`: the pull request project itself).
/// `None` when its upstream isn't on the same forge host.
fn head_of(b: &LocalBranch, remotes: &RepoRemotes, urls: &[(String, String)]) -> Option<(String, Option<String>)> {
    let project = |url: &RemoteUrl| (!url.path.eq_ignore_ascii_case(&remotes.pr_url.path)).then(|| url.path.clone());
    match (&b.upstream_remote, &b.upstream_branch) {
        // Tracking a pull request's own ref (a fork's, checked out without a remote for it).
        (Some(_), Some(branch)) if branch.starts_with("refs/") => None,
        (Some(remote), Some(branch)) => {
            let url = urls.iter().find(|(n, _)| n == remote).and_then(|(_, u)| parse_remote_url(u))?;
            (url.host == remotes.pr_url.host).then(|| (branch.clone(), project(&url)))
        }
        // The current branch before its first push: where it would be pushed.
        _ if b.current => Some((b.name.clone(), remotes.push.as_ref().and_then(|(_, u)| project(u)))),
        _ => None,
    }
}

/// The branch names to ask the forge about: the current branch's first, then
/// every other branch with an upstream on the forge.
pub fn query_branches(branches: &[LocalBranch], remotes: &RepoRemotes, urls: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let ordered = branches.iter().filter(|b| b.current).chain(branches.iter().filter(|b| !b.current));
    for b in ordered {
        if let Some((name, _)) = head_of(b, remotes, urls)
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// The pull request for a branch: an open (or draft) one first, else the most
/// recently merged, else the most recently closed.
pub fn match_pr<'a>(prs: &'a HashMap<String, Vec<PullRequest>>, b: &LocalBranch, remotes: &RepoRemotes, urls: &[(String, String)]) -> Option<&'a PullRequest> {
    let (head, fork) = head_of(b, remotes, urls)?;
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    prs.get(&head)?
        .iter()
        .filter(|p| match (&fork, &p.head_repo) {
            (None, None) => true,
            (None, Some(h)) => same(h, &remotes.pr_url.path),
            (Some(f), Some(h)) => same(f, h),
            (Some(_), None) => false,
        })
        .min_by_key(|p| {
            let rank = match p.state {
                PrState::Open | PrState::Draft => 0,
                PrState::Merged => 1,
                PrState::Closed => 2,
            };
            (rank, std::cmp::Reverse(p.updated))
        })
}

/// A branch's pull request, with where it's from and how fresh the answer is.
#[derive(Clone, Debug, PartialEq)]
pub struct PrStatus {
    pub pr: PullRequest,
    pub kind: ForgeKind,
    /// When the forge was asked (Unix seconds).
    pub as_of: i64,
    /// The local branch is still where the pull request's branch was (so a
    /// merged one can be deleted without losing anything).
    pub tip_matches: bool,
}

/// Every local branch's pull request.
pub fn statuses(prs: &HashMap<String, Vec<PullRequest>>, branches: &[LocalBranch], remotes: &RepoRemotes, urls: &[(String, String)], kind: ForgeKind, as_of: i64) -> HashMap<String, PrStatus> {
    branches
        .iter()
        .filter_map(|b| {
            let pr = match_pr(prs, b, remotes, urls)?;
            let tip_matches = matches!((&b.tip, &pr.head_oid), (Some(t), Some(h)) if t == h);
            Some((b.name.clone(), PrStatus { pr: pr.clone(), kind, as_of, tip_matches }))
        })
        .collect()
}

/// Branches whose pull request was merged, by name.
pub fn merged_branches(statuses: &HashMap<String, PrStatus>) -> Vec<String> {
    let mut v: Vec<String> = statuses.iter().filter(|(_, s)| s.pr.state == PrState::Merged).map(|(n, _)| n.clone()).collect();
    v.sort();
    v
}

impl PrStatus {
    /// Merged or closed: the work on this branch is done with.
    pub fn finished(&self) -> bool {
        matches!(self.pr.state, PrState::Merged | PrState::Closed)
    }

    /// The badge: a shape for the state, then the number ("○ #12", "✓ #9 merged").
    pub fn badge(&self) -> String {
        let n = self.pr.number;
        match self.pr.state {
            PrState::Open => format!("○ #{n}"),
            PrState::Draft => format!("◌ #{n} draft"),
            PrState::Merged => format!("✓ #{n} merged"),
            PrState::Closed => format!("✕ #{n} closed"),
        }
    }

    /// The check mark next to the badge, while the pull request is still open.
    pub fn check_mark(&self) -> Option<&'static str> {
        if self.finished() {
            return None;
        }
        match self.pr.checks.state {
            CheckState::Passing => Some("✔"),
            CheckState::Failing => Some("✖"),
            CheckState::Pending => Some("●"),
            CheckState::None => None,
        }
    }

    pub fn checks_words(&self) -> Option<&'static str> {
        match self.pr.checks.state {
            CheckState::Passing => Some("checks passing"),
            CheckState::Failing => Some("checks failing"),
            CheckState::Pending => Some("checks running"),
            CheckState::None => None,
        }
    }

    pub fn review_words(&self) -> Option<&'static str> {
        match self.pr.review {
            ReviewDecision::Approved => Some("approved"),
            ReviewDecision::ChangesRequested => Some("changes requested"),
            ReviewDecision::ReviewRequired => Some("review required"),
            ReviewDecision::None => None,
        }
    }

    /// One line for the current branch: "#123 · draft · checks failing · changes requested".
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("#{}", self.pr.number)];
        match self.pr.state {
            PrState::Draft => parts.push("draft".into()),
            PrState::Merged => parts.push("merged".into()),
            PrState::Closed => parts.push("closed".into()),
            PrState::Open => {}
        }
        if !self.finished() {
            parts.extend(self.checks_words().map(String::from));
            parts.extend(self.review_words().map(String::from));
        }
        parts.join(" · ")
    }

    /// "12 passing, 1 failing, 2 pending" (skipped ones only when nothing else ran).
    pub fn check_counts(&self) -> Option<String> {
        let c = &self.pr.checks;
        let mut parts = Vec::new();
        for (n, word) in [(c.passing, "passing"), (c.failing, "failing"), (c.pending, "pending")] {
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        if c.none > 0 {
            parts.push(format!("{} skipped", c.none));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Everything about it, for a tooltip. `relative`: how long ago a Unix time was.
    pub fn tooltip(&self, relative: impl Fn(i64) -> String) -> String {
        format!("{}\nClick to open on {}", self.details(relative), self.kind.name())
    }

    /// [`Self::tooltip`] without saying what a click does.
    pub fn details(&self, relative: impl Fn(i64) -> String) -> String {
        let pr = &self.pr;
        let what = match pr.state {
            PrState::Open => "Pull request",
            PrState::Draft => "Draft pull request",
            PrState::Merged => "Merged pull request",
            PrState::Closed => "Closed pull request",
        };
        let what = if self.kind == ForgeKind::GitLab { what.replace("pull request", "merge request").replace("Pull request", "Merge request") } else { what.to_string() };
        let mut lines = vec![format!("{what} #{}: {}", pr.number, pr.title)];
        let from = pr.head_repo.as_ref().map_or_else(|| pr.head_branch.clone(), |r| format!("{r}:{}", pr.head_branch));
        lines.push(format!("By {} · {from} → {}", pr.author, pr.base_branch));
        if !self.finished() {
            let mut review = self.review_words().map(capitalise).unwrap_or_else(|| "No review yet".into());
            if !pr.requested_reviewers.is_empty() {
                review += &format!(" · waiting on {}", pr.requested_reviewers.join(", "));
            }
            lines.push(review);
            if let Some(c) = self.check_counts() {
                lines.push(format!("Checks: {c}"));
            }
        }
        lines.push(format!("Updated {} · as of {}", relative(pr.updated), relative(self.as_of)));
        lines.join("\n")
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// The remote for a pull request's head repository: the pull request remote
/// itself, or one pointing at the fork. `None`: the fork has no remote here.
pub fn head_remote(pr: &PullRequest, remotes: &RepoRemotes, urls: &[(String, String)]) -> Option<String> {
    let Some(fork) = &pr.head_repo else { return Some(remotes.pr_remote.clone()) };
    urls.iter()
        .find(|(_, u)| parse_remote_url(u).is_some_and(|u| u.host == remotes.pr_url.host && u.path.eq_ignore_ascii_case(fork)))
        .map(|(n, _)| n.clone())
}

/// Everything git needs to fetch and check out a pull request.
pub fn pr_source(pr: &PullRequest, kind: ForgeKind, remotes: &RepoRemotes, urls: &[(String, String)]) -> git::PrSource {
    git::PrSource {
        number: pr.number,
        remote: remotes.pr_remote.clone(),
        head_ref: kind.pr_head_ref(pr.number),
        head_branch: pr.head_branch.clone(),
        head_remote: head_remote(pr, remotes, urls),
    }
}

/// For a pull request from a fork with no remote here: a name and address for
/// one, written like the pull request remote's (https or ssh).
pub fn fork_remote(pr: &PullRequest, remotes: &RepoRemotes, urls: &[(String, String)]) -> Option<(String, String)> {
    let fork = pr.head_repo.as_ref().filter(|_| head_remote(pr, remotes, urls).is_none())?;
    let base = urls.iter().find(|(n, _)| *n == remotes.pr_remote).map(|(_, u)| u.as_str()).unwrap_or("");
    let url = match base.find(remotes.pr_url.path.as_str()) {
        Some(i) => format!("{}{fork}{}", &base[..i], &base[i + remotes.pr_url.path.len()..]),
        None => format!("https://{}/{fork}.git", remotes.pr_url.host),
    };
    let owner = fork.split('/').next().unwrap_or(fork).to_ascii_lowercase();
    let name = if urls.iter().any(|(n, _)| *n == owner) { format!("{owner}-fork") } else { owner };
    Some((name, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, branch: &str, state: PrState, head_repo: Option<&str>, updated: i64) -> PullRequest {
        PullRequest {
            number,
            title: format!("PR {number}"),
            state,
            author: "sam".into(),
            head_branch: branch.into(),
            base_branch: "main".into(),
            head_repo: head_repo.map(String::from),
            url: format!("https://github.com/acme/shop/pull/{number}"),
            review: ReviewDecision::None,
            requested_reviewers: vec![],
            checks: CheckSummary::default(),
            mergeable: Mergeable::Unknown,
            updated,
            head_oid: None,
        }
    }

    fn by_branch(list: Vec<PullRequest>) -> HashMap<String, Vec<PullRequest>> {
        let mut m: HashMap<String, Vec<PullRequest>> = HashMap::new();
        for p in list {
            m.entry(p.head_branch.clone()).or_default().push(p);
        }
        m
    }

    fn branch(name: &str, upstream: Option<(&str, &str)>, current: bool) -> LocalBranch {
        LocalBranch { name: name.into(), upstream_remote: upstream.map(|u| u.0.into()), upstream_branch: upstream.map(|u| u.1.into()), current, tip: None }
    }

    fn urls(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(n, u)| (n.to_string(), u.to_string())).collect()
    }

    #[test]
    fn branches_match_by_their_upstream_not_their_local_name() {
        let u = urls(&[("origin", "git@github.com:acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        let prs = by_branch(vec![pr(1, "fix-login", PrState::Open, None, 5), pr(2, "fix", PrState::Open, None, 5)]);
        let fix = branch("fix", Some(("origin", "fix-login")), false);
        assert_eq!(match_pr(&prs, &fix, &remotes, &u).unwrap().number, 1);
        // No upstream and not current: nothing to go on.
        assert_eq!(match_pr(&prs, &branch("fix", None, false), &remotes, &u), None);
        // The current branch before its first push goes by its own name.
        assert_eq!(match_pr(&prs, &branch("fix", None, true), &remotes, &u).unwrap().number, 2);
        // What to ask: upstream names, the current branch's first, once each.
        let all = [branch("a", Some(("origin", "fix-login")), false), branch("b", Some(("origin", "fix-login")), false), branch("wip", None, true), branch("x", None, false)];
        assert_eq!(query_branches(&all, &remotes, &u), ["wip", "fix-login"]);
    }

    #[test]
    fn pull_requests_are_fetched_from_their_project_and_tracked_from_their_fork() {
        let u = urls(&[("origin", "git@github.com:ana/shop.git"), ("upstream", "https://github.com/acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        // From the project itself: fetched and tracked from upstream.
        let own = pr(12, "fix", PrState::Open, None, 1);
        let src = pr_source(&own, ForgeKind::GitHub, &remotes, &u);
        assert_eq!((src.remote.as_str(), src.head_ref.as_str(), src.head_remote.as_deref()), ("upstream", "refs/pull/12/head", Some("upstream")));
        assert_eq!(fork_remote(&own, &remotes, &u), None);
        // From my fork (origin): tracked from origin, owner names compare without case.
        let mine = pr(13, "main", PrState::Open, Some("Ana/Shop"), 1);
        assert_eq!(pr_source(&mine, ForgeKind::GitLab, &remotes, &u).head_remote.as_deref(), Some("origin"));
        assert_eq!(pr_source(&mine, ForgeKind::GitLab, &remotes, &u).head_ref, "refs/merge-requests/13/head");
        // From someone else's fork: no remote yet, so one is offered, written like upstream's.
        let theirs = pr(14, "main", PrState::Open, Some("bob/shop"), 1);
        assert_eq!(pr_source(&theirs, ForgeKind::GitHub, &remotes, &u).head_remote, None);
        assert_eq!(fork_remote(&theirs, &remotes, &u), Some(("bob".into(), "https://github.com/bob/shop.git".into())));
        let ssh = urls(&[("origin", "git@github.com:acme/shop.git"), ("bob", "git@github.com:other/x.git")]);
        assert_eq!(fork_remote(&theirs, &pick_remotes(&ssh).unwrap(), &ssh), Some(("bob-fork".into(), "git@github.com:bob/shop.git".into())));
        // A branch tracking a pull request's ref isn't asked about by name.
        assert!(query_branches(&[branch("pr/14", Some(("upstream", "refs/pull/14/head")), false)], &remotes, &u).is_empty());
    }

    #[test]
    fn forks_match_the_head_project_too() {
        let u = urls(&[("origin", "git@github.com:ana/shop.git"), ("upstream", "https://github.com/acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        let prs = by_branch(vec![
            pr(10, "fix", PrState::Open, Some("ana/shop"), 1),
            pr(11, "fix", PrState::Open, Some("bob/shop"), 9),
            pr(12, "fix", PrState::Open, None, 9),
            pr(13, "main", PrState::Merged, Some("Ana/Shop"), 1),
        ]);
        // Pushed to my fork: my fork's pull request, not someone else's of the same name.
        assert_eq!(match_pr(&prs, &branch("fix", Some(("origin", "fix")), false), &remotes, &u).unwrap().number, 10);
        // Tracking the original project's branch: the one from the project itself.
        assert_eq!(match_pr(&prs, &branch("fix", Some(("upstream", "fix")), false), &remotes, &u).unwrap().number, 12);
        // Owner names compare without case.
        assert_eq!(match_pr(&prs, &branch("main", Some(("origin", "main")), false), &remotes, &u).unwrap().number, 13);
        // The current branch, not pushed yet, would go to the fork.
        assert_eq!(match_pr(&prs, &branch("fix", None, true), &remotes, &u).unwrap().number, 10);
    }

    #[test]
    fn upstreams_elsewhere_have_no_pull_request() {
        let u = urls(&[("origin", "git@github.com:acme/shop.git"), ("backup", "/mnt/backup/shop.git"), ("lab", "git@gitlab.com:acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        let prs = by_branch(vec![pr(1, "fix", PrState::Open, None, 1)]);
        assert_eq!(match_pr(&prs, &branch("fix", Some(("backup", "fix")), false), &remotes, &u), None);
        assert_eq!(match_pr(&prs, &branch("fix", Some(("lab", "fix")), false), &remotes, &u), None, "another host");
        assert_eq!(match_pr(&prs, &branch("fix", Some(("gone", "fix")), false), &remotes, &u), None, "a remote that no longer exists");
        assert_eq!(match_pr(&prs, &branch("fix", Some(("origin", "fix")), false), &remotes, &u).unwrap().number, 1);
    }

    #[test]
    fn open_beats_merged_beats_closed_then_newest() {
        let u = urls(&[("origin", "git@github.com:acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        let b = branch("f", Some(("origin", "f")), false);
        let pick = |list| match_pr(&by_branch(list), &b, &remotes, &u).map(|p| p.number);
        assert_eq!(pick(vec![pr(1, "f", PrState::Closed, None, 9), pr(2, "f", PrState::Merged, None, 1), pr(3, "f", PrState::Draft, None, 0)]), Some(3));
        assert_eq!(pick(vec![pr(1, "f", PrState::Closed, None, 9), pr(2, "f", PrState::Merged, None, 1)]), Some(2));
        assert_eq!(pick(vec![pr(1, "f", PrState::Closed, None, 1), pr(2, "f", PrState::Closed, None, 5)]), Some(2));
    }

    #[test]
    fn badges_for_each_sample_pull_request() {
        let data = crate::samples::web_shop_forge(1_000_000);
        let pulls = &data.repos["github.com/headway-samples/web-shop"].pulls;
        let st = |n: u64| PrStatus { pr: pulls.iter().find(|p| p.number == n).unwrap().clone(), kind: ForgeKind::GitHub, as_of: 1_000_000, tip_matches: false };
        let rel = |t: i64| format!("{}s", 1_000_000 - t);

        let open = st(12);
        assert_eq!((open.badge().as_str(), open.check_mark()), ("○ #12", Some("✔")));
        assert_eq!(open.summary(), "#12 · checks passing · approved");

        let draft = st(14);
        assert_eq!((draft.badge().as_str(), draft.check_mark()), ("◌ #14 draft", Some("●")));
        assert_eq!(draft.summary(), "#14 · draft · checks running");
        assert!(draft.tooltip(rel).contains("No review yet · waiting on jo-lindqvist"));

        let failing = st(13);
        assert_eq!((failing.badge().as_str(), failing.check_mark()), ("○ #13", Some("✖")));
        assert_eq!(failing.summary(), "#13 · checks failing · changes requested");
        assert_eq!(failing.check_counts().as_deref(), Some("2 passing, 1 failing, 1 skipped"));
        assert_eq!(
            failing.tooltip(rel),
            "Pull request #13: Round cart totals to cents\nBy alex-rivera · fix/tax-rounding → main\nChanges requested · waiting on sam-okafor\nChecks: 2 passing, 1 failing, 1 skipped\nUpdated 7200s · as of 0s\nClick to open on GitHub"
        );

        let merged = st(9);
        assert!(merged.finished());
        assert_eq!((merged.badge().as_str(), merged.check_mark()), ("✓ #9 merged", None));
        assert_eq!(merged.summary(), "#9 · merged");
        assert!(merged.tooltip(rel).starts_with("Merged pull request #9"));
        assert!(!merged.tooltip(rel).contains("Checks:"), "finished: no checks or reviews");
    }

    #[test]
    fn merged_branches_and_whether_the_branch_moved_on() {
        let u = urls(&[("origin", "git@github.com:acme/shop.git")]);
        let remotes = pick_remotes(&u).unwrap();
        let mut done = pr(9, "old", PrState::Merged, None, 1);
        done.head_oid = Some("abc".into());
        let prs = by_branch(vec![done, pr(8, "dead", PrState::Closed, None, 1), pr(7, "live", PrState::Open, None, 1)]);
        let mut old = branch("old", Some(("origin", "old")), false);
        old.tip = Some("abc".into());
        let mut moved = branch("old-copy", Some(("origin", "old")), false);
        moved.tip = Some("def".into());
        let all = [old, moved, branch("dead", Some(("origin", "dead")), false), branch("live", Some(("origin", "live")), false)];
        let s = statuses(&prs, &all, &remotes, &u, ForgeKind::GitHub, 0);
        assert_eq!(merged_branches(&s), ["old", "old-copy"]);
        assert!(s["old"].tip_matches);
        assert!(!s["old-copy"].tip_matches, "new commits since the merge");
        assert!(s["dead"].finished() && !s["live"].finished());
        let gl = PrStatus { kind: ForgeKind::GitLab, ..s["live"].clone() };
        assert!(gl.tooltip(|_| String::new()).starts_with("Merge request #7"));
        assert!(gl.tooltip(|_| String::new()).ends_with("Click to open on GitLab"));
    }
}
