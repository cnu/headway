//! The latest forge results, and the rules for when to ask again.
//!
//! The app keeps one [`ForgeCache`]. It asks [`ForgeCache::request`] whenever
//! something happened that may have changed a repository's pull requests (a tab
//! opened, focus came back, a fetch or push finished, the poll timer); the cache
//! decides whether that's worth a call, queues a [`Job`], and hands out at most
//! [`MAX_IN_FLIGHT`] at a time from [`ForgeCache::next_jobs`]. Jobs run on worker
//! threads ([`run_job`]) and come back through [`ForgeCache::finish`]. Nothing
//! here waits or starts threads, and every rule takes `now`, so tests can move
//! the clock.

use super::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Forge jobs running at once, across the app.
pub const MAX_IN_FLIGHT: usize = 4;
/// The window coming back into focus refreshes a repository at most this often.
pub const FOCUS_EVERY: Duration = Duration::from_secs(60);
/// Visible repositories are refreshed this often while the window has focus.
pub const POLL_EVERY: Duration = Duration::from_secs(180);
/// How long to leave a host alone after it said "too many requests" without saying until when.
pub const GITHUB_BACKOFF: Duration = Duration::from_secs(15 * 60);
pub const GITLAB_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// Never back off longer than this, whatever the forge says.
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// Branches asked about per repository (the current one first).
pub const MAX_BRANCHES: usize = 90;

/// Why a refresh is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A repository tab was opened.
    Opened,
    /// The window came back into focus.
    Focus,
    /// A fetch, pull or push finished.
    AfterGit,
    /// A pull request was created, merged, closed…
    AfterPrAction,
    /// The timer, for repositories on screen.
    Poll,
}

/// Whether a host's CLI can be used, and as whom.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Connection {
    #[default]
    Unknown,
    SignedIn(String),
    NotInstalled,
    SignedOut,
    /// The CLI is there but `whoami` failed for another reason.
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct HostEntry {
    /// Which forge (once detected; `None` after detection: not one we know).
    pub kind: Option<ForgeKind>,
    pub detected: bool,
    pub connection: Connection,
    /// Nothing is sent to this host before then (rate limits).
    pub backoff_until: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct RepoEntry {
    pub info: Option<RepoInfo>,
    /// Pull requests per branch, as of `fetched`.
    pub prs: HashMap<String, Vec<PullRequest>>,
    pub fetched: Option<Instant>,
    /// The same moment as Unix seconds, to say how old the data is.
    pub fetched_at: i64,
    /// The last refresh failed with this (the data above is older).
    pub error: Option<ForgeError>,
    /// When a refresh was last asked for (whether or not it worked).
    asked: Option<Instant>,
    /// The sidebar's lists (asked for only while the repository is open in a tab),
    /// as of `lists_at` (Unix seconds).
    pub lists: Option<Arc<PrLists>>,
    pub lists_at: i64,
    /// Queued or running.
    busy: bool,
    /// The app wants the lists too (see [`ForgeCache::want_lists`]).
    lists_wanted: bool,
    /// The running job leaves the lists out.
    running_without_lists: bool,
    /// Asked for again while a job without the lists was running: queue this next.
    again: Option<(PathBuf, Vec<String>)>,
}

/// One refresh of one repository: whatever is still unknown about its host, then
/// its pull requests.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub key: RepoKey,
    /// The folder asking, for the command log.
    pub dir: PathBuf,
    pub branches: Vec<String>,
    pub kind: Option<ForgeKind>,
    pub detect: bool,
    pub whoami: bool,
    pub info: bool,
    /// Read the sidebar's lists too, as this user (filled in when signed in).
    pub lists: bool,
    pub me: Option<String>,
}

#[derive(Debug, Default)]
pub struct JobResult {
    pub key: Option<RepoKey>,
    pub host_kind: Option<HostKind>,
    pub user: Option<ForgeResult<String>>,
    pub info: Option<ForgeResult<RepoInfo>>,
    pub prs: Option<ForgeResult<BranchPrs>>,
    pub lists: Option<ForgeResult<PrLists>>,
}

/// One host's inbox: the latest answer and when to ask again.
#[derive(Clone, Debug, Default)]
pub struct InboxEntry {
    pub inbox: Option<Arc<Inbox>>,
    /// Unix seconds of the answer.
    pub fetched_at: i64,
    /// The last refresh failed with this (the inbox above is older).
    pub error: Option<ForgeError>,
    asked: Option<Instant>,
    busy: bool,
}

/// Reading one host's inbox, as the signed-in user, for these repositories.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxJob {
    pub host: String,
    pub kind: ForgeKind,
    pub me: String,
    pub repos: Vec<RepoKey>,
    /// A folder on that host, for the command log.
    pub dir: PathBuf,
    /// Merged ones since then (Unix seconds: when the app started).
    pub since: i64,
}

#[derive(Debug)]
pub struct InboxResult {
    pub host: String,
    pub inbox: ForgeResult<Inbox>,
}

#[derive(Default)]
pub struct ForgeCache {
    hosts: HashMap<String, HostEntry>,
    repos: HashMap<RepoKey, RepoEntry>,
    queue: VecDeque<Job>,
    running: Vec<RepoKey>,
    inboxes: HashMap<String, InboxEntry>,
    inbox_queue: VecDeque<InboxJob>,
    running_inboxes: Vec<String>,
}

impl ForgeCache {
    pub fn host(&self, host: &str) -> Option<&HostEntry> {
        self.hosts.get(host)
    }

    pub fn repo(&self, key: &RepoKey) -> Option<&RepoEntry> {
        self.repos.get(key)
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.running.len()
    }

    /// Whether refreshes of `key` should read its pull request lists too (it's
    /// open in a tab). Overview cards only need their branches.
    pub fn want_lists(&mut self, key: &RepoKey, on: bool) {
        self.repos.entry(key.clone()).or_default().lists_wanted = on;
    }

    fn backed_off(&self, host: &str, now: Instant) -> bool {
        self.hosts.get(host).and_then(|h| h.backoff_until).is_some_and(|t| now < t)
    }

    /// Ask for a refresh of `key` (on behalf of the folder `dir`, for `branches`).
    /// Queues a job and returns true when the rules say it's due.
    pub fn request(&mut self, key: &RepoKey, dir: &Path, branches: &[String], reason: Reason, now: Instant) -> bool {
        if self.backed_off(&key.host, now) {
            return false;
        }
        if let Some(h) = self.hosts.get(&key.host)
            && h.detected
            && h.kind.is_none()
        {
            return false; // Not a forge Headway knows.
        }
        let entry = self.repos.entry(key.clone()).or_default();
        let wanted = || {
            let mut w: Vec<String> = Vec::new();
            for b in branches {
                if !w.contains(b) && w.len() < MAX_BRANCHES {
                    w.push(b.clone());
                }
            }
            w
        };
        if entry.busy {
            // A card's refresh is running and the tab that just opened wants the lists.
            if entry.running_without_lists && entry.lists_wanted && (entry.lists.is_none() || matches!(reason, Reason::Opened | Reason::AfterGit | Reason::AfterPrAction)) {
                entry.again = Some((dir.to_path_buf(), wanted()));
            }
            return false;
        }
        let since = |d: Duration| entry.asked.is_none_or(|t| now.saturating_duration_since(t) >= d);
        let due = match reason {
            Reason::Opened | Reason::AfterGit | Reason::AfterPrAction => true,
            Reason::Focus => since(FOCUS_EVERY),
            Reason::Poll => since(POLL_EVERY),
        };
        if !due {
            return false;
        }
        entry.asked = Some(now);
        entry.busy = true;
        let branches = wanted();
        self.queue.push_back(Job::new(key, dir, branches));
        true
    }

    /// Jobs to start now (keeping at most [`MAX_IN_FLIGHT`] running), with what
    /// each still has to find out. A host is set up (detected, signed in) by one
    /// job at a time; others for it wait their turn.
    pub fn next_jobs(&mut self, now: Instant) -> Vec<Job> {
        let mut out = Vec::new();
        let mut i = 0;
        while self.in_flight() < MAX_IN_FLIGHT && i < self.queue.len() {
            let host = self.queue[i].key.host.clone();
            if self.backed_off(&host, now) {
                // Dropped: the host is resting. The poll asks again later.
                let job = self.queue.remove(i).expect("index checked");
                if let Some(e) = self.repos.get_mut(&job.key) {
                    e.busy = false;
                }
                continue;
            }
            let h = self.hosts.entry(host.clone()).or_default();
            let settled = h.detected && matches!(h.connection, Connection::SignedIn(_));
            if !settled && self.running.iter().any(|k| k.host == host) {
                i += 1;
                continue;
            }
            let mut job = self.queue.remove(i).expect("index checked");
            let h = &self.hosts[&host];
            job.kind = h.kind;
            job.detect = !h.detected;
            job.whoami = !matches!(h.connection, Connection::SignedIn(_));
            job.me = match &h.connection {
                Connection::SignedIn(u) => Some(u.clone()),
                _ => None,
            };
            let entry = self.repos.entry(job.key.clone()).or_default();
            job.info = entry.info.is_none();
            job.lists = entry.lists_wanted;
            entry.running_without_lists = !job.lists;
            self.running.push(job.key.clone());
            out.push(job);
        }
        out
    }

    fn in_flight(&self) -> usize {
        self.running.len() + self.running_inboxes.len()
    }

    pub fn inbox(&self, host: &str) -> Option<&InboxEntry> {
        self.inboxes.get(host)
    }

    /// Ask for a host's inbox over `repos` (on behalf of the folder `dir`).
    /// Only once the host is known and signed in (repository refreshes find
    /// that out); then the same rules as a repository's refresh. Asking about
    /// other repositories than last time (another workspace) is always due.
    pub fn request_inbox(&mut self, host: &str, repos: &[RepoKey], dir: &Path, since: i64, reason: Reason, now: Instant) -> bool {
        if repos.is_empty() || self.backed_off(host, now) {
            return false;
        }
        let Some(h) = self.hosts.get(host) else { return false };
        let (Some(kind), Connection::SignedIn(me)) = (h.kind, &h.connection) else { return false };
        let me = me.clone();
        let entry = self.inboxes.entry(host.to_string()).or_default();
        if entry.busy {
            return false;
        }
        let same = entry.inbox.as_ref().is_some_and(|i| i.repos == repos);
        let since_asked = |d: Duration| entry.asked.is_none_or(|t| now.saturating_duration_since(t) >= d);
        let due = !same
            || match reason {
                Reason::Opened | Reason::AfterGit | Reason::AfterPrAction => true,
                Reason::Focus => since_asked(FOCUS_EVERY),
                Reason::Poll => since_asked(POLL_EVERY),
            };
        if !due {
            return false;
        }
        entry.asked = Some(now);
        entry.busy = true;
        self.inbox_queue.push_back(InboxJob { host: host.to_string(), kind, me, repos: repos.to_vec(), dir: dir.to_path_buf(), since });
        true
    }

    /// Inbox reads to start now, within the same [`MAX_IN_FLIGHT`] as
    /// repository refreshes (those go first).
    pub fn next_inbox_jobs(&mut self, now: Instant) -> Vec<InboxJob> {
        let mut out = Vec::new();
        while self.in_flight() < MAX_IN_FLIGHT
            && let Some(job) = self.inbox_queue.pop_front()
        {
            if self.backed_off(&job.host, now) {
                if let Some(e) = self.inboxes.get_mut(&job.host) {
                    e.busy = false;
                }
                continue;
            }
            self.running_inboxes.push(job.host.clone());
            out.push(job);
        }
        out
    }

    /// Take in an inbox. Returns the inbox it replaces (to tell what changed)
    /// and, as [`ForgeCache::finish`] does, a line for the command log when
    /// the host starts resting.
    pub fn finish_inbox(&mut self, res: InboxResult, now: Instant, wall_now: i64) -> (Option<Arc<Inbox>>, Option<String>) {
        self.running_inboxes.retain(|h| *h != res.host);
        let entry = self.inboxes.entry(res.host.clone()).or_default();
        entry.busy = false;
        let before = entry.inbox.clone();
        let (limited, slow_down) = match res.inbox {
            Ok(i) => {
                let slow = i.slow_down_until;
                entry.inbox = Some(Arc::new(i));
                entry.fetched_at = wall_now;
                entry.error = None;
                (None, slow)
            }
            Err(e) => {
                let limited = match &e {
                    ForgeError::RateLimited { until } => Some(*until),
                    _ => None,
                };
                let host = self.hosts.entry(res.host.clone()).or_default();
                match &e {
                    ForgeError::SignedOut { .. } => host.connection = Connection::SignedOut,
                    ForgeError::NotInstalled(_) => host.connection = Connection::NotInstalled,
                    _ => {}
                }
                self.inboxes.get_mut(&res.host).expect("just added").error = Some(e);
                (limited, None)
            }
        };
        (before, self.rest(&res.host, limited, slow_down, now, wall_now))
    }

    /// Back off from `host` after a rate limit (`limited`: until when, if it
    /// said) or when nearly out of requests (`slow_down`). Returns the line for
    /// the command log when it starts resting.
    fn rest(&mut self, host_name: &str, limited: Option<Option<i64>>, slow_down: Option<i64>, now: Instant, wall_now: i64) -> Option<String> {
        let host = self.hosts.entry(host_name.to_string()).or_default();
        let (until, why) = match (limited, slow_down) {
            (Some(until), _) => {
                let default = if host.kind == Some(ForgeKind::GitLab) { GITLAB_BACKOFF } else { GITHUB_BACKOFF };
                (until.map_or(default, |t| secs_until(t, wall_now)), "rate limited")
            }
            (None, Some(t)) => (secs_until(t, wall_now), "nearly out of requests"),
            (None, None) => return None,
        };
        let resting = host.backoff_until.is_some_and(|t| now < t);
        host.backoff_until = Some(now + until.min(MAX_BACKOFF));
        (!resting).then(|| format!("{host_name}: {why}; not asking again for {} min", until.min(MAX_BACKOFF).as_secs().div_ceil(60)))
    }

    /// Take in a job's results. `wall_now` is the Unix time (for reset times the
    /// forge gives). Returns a line for the command log when the host starts
    /// resting because of rate limits (once per rest, never a toast).
    pub fn finish(&mut self, res: JobResult, now: Instant, wall_now: i64) -> Option<String> {
        let key = res.key?;
        self.running.retain(|k| *k != key);
        let host = self.hosts.entry(key.host.clone()).or_default();
        if let Some(hk) = &res.host_kind {
            host.kind = hk.kind;
            host.detected = true;
        }
        let mut limited: Option<Option<i64>> = None;
        let mut signed_out = |e: &ForgeError, host: &mut HostEntry| match e {
            ForgeError::NotInstalled(_) => host.connection = Connection::NotInstalled,
            ForgeError::SignedOut { .. } => host.connection = Connection::SignedOut,
            ForgeError::RateLimited { until } => limited = Some(*until),
            _ => {}
        };
        match &res.user {
            Some(Ok(u)) => host.connection = Connection::SignedIn(u.clone()),
            Some(Err(e @ (ForgeError::NotInstalled(_) | ForgeError::SignedOut { .. } | ForgeError::RateLimited { .. }))) => signed_out(e, host),
            Some(Err(e)) => host.connection = Connection::Failed(e.to_string()),
            None => {}
        }
        let entry = self.repos.entry(key.clone()).or_default();
        entry.busy = false;
        entry.running_without_lists = false;
        if let Some((dir, branches)) = entry.again.take() {
            entry.busy = true;
            self.queue.push_back(Job::new(&key, &dir, branches));
        }
        let entry = self.repos.get_mut(&key).expect("just added");
        if let Some(Ok(info)) = &res.info {
            entry.info = Some(info.clone());
        }
        let mut slow_down = None;
        match &res.prs {
            Some(Ok(p)) => {
                entry.prs = p.by_branch.clone();
                entry.fetched = Some(now);
                entry.fetched_at = wall_now;
                entry.error = None;
                slow_down = p.slow_down_until;
            }
            Some(Err(e)) => {
                entry.error = Some(e.clone());
                signed_out(e, host);
            }
            None => {}
        }
        match &res.lists {
            Some(Ok(l)) => {
                entry.lists = Some(Arc::new(l.clone()));
                entry.lists_at = wall_now;
                slow_down = slow_down.max(l.slow_down_until);
            }
            Some(Err(e)) => {
                entry.error = Some(e.clone());
                signed_out(e, host);
            }
            None => {}
        }
        if let Some(Err(e)) = &res.info {
            signed_out(e, host);
        }
        self.rest(&key.host, limited, slow_down, now, wall_now)
    }
}

impl ForgeCache {
    /// A pull request was just opened from Headway: show it at once, before the
    /// refresh that follows comes back.
    pub fn add_created(&mut self, key: &RepoKey, pr: &PullRequest, now: Instant, wall_now: i64) {
        let entry = self.repos.entry(key.clone()).or_default();
        let list = entry.prs.entry(pr.head_branch.clone()).or_default();
        list.retain(|p| p.number != pr.number);
        list.insert(0, pr.clone());
        if entry.fetched.is_none() {
            entry.fetched = Some(now);
            entry.fetched_at = wall_now;
        }
        if let Some(lists) = &entry.lists {
            let mut l = (**lists).clone();
            if !l.open.iter().any(|p| p.number == pr.number) {
                l.mine.insert(0, pr.clone());
                l.open.insert(0, pr.clone());
                l.open_total += 1;
            }
            entry.lists = Some(Arc::new(l));
        }
    }
}

impl Job {
    fn new(key: &RepoKey, dir: &Path, branches: Vec<String>) -> Self {
        Job { key: key.clone(), dir: dir.to_path_buf(), branches, kind: None, detect: false, whoami: false, info: false, lists: false, me: None }
    }
}

fn secs_until(t: i64, wall_now: i64) -> Duration {
    Duration::from_secs(t.saturating_sub(wall_now).max(60) as u64)
}

/// Read an inbox (on a worker thread).
pub fn run_inbox_job(forges: &dyn Forges, job: &InboxJob) -> InboxResult {
    let forge = forges.forge(job.kind, &job.host, Some(&job.dir));
    InboxResult { host: job.host.clone(), inbox: forge.inbox(&job.me, &job.repos, job.since) }
}

/// Do a job (on a worker thread): detect the host and check the sign-in if still
/// needed, then read the repository's settings and pull requests. Stops early
/// when the host isn't a forge or the CLI can't be used.
pub fn run_job(forges: &dyn Forges, job: &Job) -> JobResult {
    let dir = Some(job.dir.as_path());
    let mut res = JobResult { key: Some(job.key.clone()), ..Default::default() };
    let mut kind = job.kind;
    if job.detect {
        let hk = forges.detect(&job.key.host, dir);
        kind = hk.kind;
        res.host_kind = Some(hk);
    }
    let Some(kind) = kind else { return res };
    let forge = forges.forge(kind, &job.key.host, dir);
    let mut me = job.me.clone();
    if job.whoami {
        let user = forge.whoami();
        let Ok(u) = &user else {
            res.user = Some(user);
            return res;
        };
        me = Some(u.clone());
        res.user = Some(user);
    }
    if job.info {
        res.info = Some(forge.repo_info(&job.key));
    }
    if !job.branches.is_empty() {
        res.prs = Some(forge.prs_for_branches(&job.key, &job.branches));
    }
    if let (true, Some(me)) = (job.lists, me) {
        res.lists = Some(forge.pr_lists(&job.key, &me, LIST_PAGE));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: &str, path: &str) -> RepoKey {
        RepoKey { host: host.into(), path: path.into() }
    }

    fn branches() -> Vec<String> {
        vec!["main".into(), "feature".into()]
    }

    /// Run every queued job at once against `forges`, like the app does.
    fn drain(c: &mut ForgeCache, forges: &dyn Forges, now: Instant) -> Vec<Job> {
        let mut done = Vec::new();
        loop {
            let jobs = c.next_jobs(now);
            if jobs.is_empty() {
                return done;
            }
            for j in jobs {
                let r = run_job(forges, &j);
                c.finish(r, now, 1_000_000);
                done.push(j);
            }
        }
    }

    fn fake() -> fake::FakeForges {
        let pr = |n: u64, b: &str| PullRequest {
            number: n,
            title: format!("PR {n}"),
            state: PrState::Open,
            author: "sam".into(),
            head_branch: b.into(),
            base_branch: "main".into(),
            head_repo: None,
            url: String::new(),
            review: ReviewDecision::None,
            requested_reviewers: vec![],
            checks: CheckSummary::default(),
            mergeable: Mergeable::Unknown,
            updated: n as i64,
            head_oid: None,
        };
        let mut data = fake::FakeData::default();
        data.hosts.insert("github.com".into(), fake::FakeHost { kind: ForgeKind::GitHub, user: Some("alex".into()), installed: true });
        let info = RepoInfo { default_branch: "main".into(), merge_methods: vec![MergeMethod::Merge], web_url: String::new() };
        for p in ["a/one", "a/two", "a/three", "a/four", "a/five", "a/six"] {
            data.repos.insert(format!("github.com/{p}"), fake::FakeRepo { info: Some(info.clone()), pulls: vec![pr(1, "feature")], ..Default::default() });
        }
        fake::FakeForges::new(data)
    }

    /// The inbox waits for the host to be signed in, keeps the usual pace, is
    /// asked again at once for other repositories, shares the jobs in flight,
    /// and rests with the host after a rate limit.
    #[test]
    fn a_hosts_inbox_follows_the_same_rules() {
        let mut c = ForgeCache::default();
        let f = fake();
        let t = Instant::now();
        let dir = Path::new("/r");
        let one = vec![key("github.com", "a/one")];
        assert!(!c.request_inbox("github.com", &one, dir, 0, Reason::Opened, t), "not known to be signed in yet");
        c.request(&one[0], dir, &branches(), Reason::Opened, t);
        drain(&mut c, &f, t);
        assert!(c.request_inbox("github.com", &one, dir, 0, Reason::Poll, t));
        assert!(!c.request_inbox("github.com", &one, dir, 0, Reason::Opened, t), "one at a time");
        let jobs = c.next_inbox_jobs(t);
        assert_eq!((jobs.len(), jobs[0].me.as_str(), jobs[0].kind), (1, "alex", ForgeKind::GitHub));
        let (before, note) = c.finish_inbox(run_inbox_job(&f, &jobs[0]), t, 1_000_000);
        assert!(before.is_none() && note.is_none(), "the first answer has nothing before it");
        assert_eq!(c.inbox("github.com").unwrap().inbox.as_ref().unwrap().repos, one);

        // The usual pace: a poll or focus soon after isn't due; asking about
        // other repositories (another workspace's) is.
        assert!(!c.request_inbox("github.com", &one, dir, 0, Reason::Poll, t + Duration::from_secs(30)));
        assert!(!c.request_inbox("github.com", &one, dir, 0, Reason::Focus, t + Duration::from_secs(30)));
        assert!(c.request_inbox("github.com", &one, dir, 0, Reason::Poll, t + POLL_EVERY));
        let jobs = c.next_inbox_jobs(t + POLL_EVERY);
        let (before, _) = c.finish_inbox(run_inbox_job(&f, &jobs[0]), t + POLL_EVERY, 1_000_000);
        assert!(before.is_some(), "what it replaces, to tell what changed");
        let two = vec![key("github.com", "a/one"), key("github.com", "a/two")];
        assert!(c.request_inbox("github.com", &two, dir, 0, Reason::Poll, t + POLL_EVERY));

        // Repository refreshes go first; the inbox waits for a free slot.
        let t2 = t + POLL_EVERY;
        for p in ["a/three", "a/four", "a/five", "a/six"] {
            c.request(&key("github.com", p), dir, &branches(), Reason::Opened, t2);
        }
        assert_eq!(c.next_jobs(t2).len(), MAX_IN_FLIGHT);
        assert!(c.next_inbox_jobs(t2).is_empty(), "all slots taken");

        // A rate limit rests the host: no inbox until it's over.
        let mut d = ForgeCache::default();
        d.request(&one[0], dir, &branches(), Reason::Opened, t);
        drain(&mut d, &f, t);
        d.request_inbox("github.com", &one, dir, 0, Reason::Opened, t);
        let job = d.next_inbox_jobs(t).remove(0);
        let (_, note) = d.finish_inbox(InboxResult { host: job.host, inbox: Err(ForgeError::RateLimited { until: None }) }, t, 1_000_000);
        assert_eq!(note.as_deref(), Some("github.com: rate limited; not asking again for 15 min"));
        assert!(!d.request_inbox("github.com", &one, dir, 0, Reason::Opened, t + Duration::from_secs(60)));
        assert!(!d.request(&one[0], dir, &branches(), Reason::Opened, t + Duration::from_secs(60)), "nor the repositories");
        assert!(d.request_inbox("github.com", &one, dir, 0, Reason::Opened, t + GITHUB_BACKOFF));
    }

    #[test]
    fn each_reason_has_its_own_pace() {
        let mut c = ForgeCache::default();
        let f = fake();
        let k = key("github.com", "a/one");
        let t0 = Instant::now();
        let dir = Path::new("/repo");

        assert!(c.request(&k, dir, &branches(), Reason::Opened, t0));
        assert!(!c.request(&k, dir, &branches(), Reason::AfterGit, t0), "already queued: once is enough");
        drain(&mut c, &f, t0);
        assert_eq!(c.repo(&k).unwrap().prs["feature"][0].number, 1);

        // Focus: at most once a minute. Poll: every three minutes.
        assert!(!c.request(&k, dir, &branches(), Reason::Focus, t0 + Duration::from_secs(30)));
        assert!(!c.request(&k, dir, &branches(), Reason::Poll, t0 + Duration::from_secs(120)));
        assert!(c.request(&k, dir, &branches(), Reason::Focus, t0 + Duration::from_secs(61)));
        drain(&mut c, &f, t0 + Duration::from_secs(61));
        assert!(!c.request(&k, dir, &branches(), Reason::Poll, t0 + Duration::from_secs(200)));
        assert!(c.request(&k, dir, &branches(), Reason::Poll, t0 + Duration::from_secs(61 + 180)));
        drain(&mut c, &f, t0 + Duration::from_secs(241));
        // Fetch, pull, push and PR actions always refresh.
        assert!(c.request(&k, dir, &branches(), Reason::AfterGit, t0 + Duration::from_secs(242)));
        drain(&mut c, &f, t0 + Duration::from_secs(242));
        assert!(c.request(&k, dir, &branches(), Reason::AfterPrAction, t0 + Duration::from_secs(243)));
    }

    #[test]
    fn the_host_is_set_up_once_then_never_more_than_four_at_a_time() {
        let mut c = ForgeCache::default();
        let f = fake();
        let t = Instant::now();
        let keys: Vec<RepoKey> = ["a/one", "a/two", "a/three", "a/four", "a/five", "a/six"].iter().map(|p| key("github.com", p)).collect();
        for k in &keys {
            assert!(c.request(k, Path::new("/r"), &branches(), Reason::Poll, t));
        }
        // Nothing is known about github.com yet: one job finds out, the rest wait.
        let first = c.next_jobs(t);
        assert_eq!(first.len(), 1);
        assert!(first[0].detect && first[0].whoami && first[0].info);
        assert!(c.next_jobs(t).is_empty(), "still waiting for the host");
        c.finish(run_job(&f, &first[0]), t, 0);
        assert_eq!(c.host("github.com").unwrap().connection, Connection::SignedIn("alex".into()));

        let next = c.next_jobs(t);
        assert_eq!(next.len(), MAX_IN_FLIGHT);
        assert!(next.iter().all(|j| !j.detect && !j.whoami && j.kind == Some(ForgeKind::GitHub)));
        assert_eq!(c.running(), 4);
        assert!(c.next_jobs(t).is_empty(), "four in flight");
        c.finish(run_job(&f, &next[0]), t, 0);
        assert_eq!(c.next_jobs(t).len(), 1, "one finished, one more starts");
    }

    #[test]
    fn rate_limits_rest_the_host_and_are_logged_once() {
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let k = key("github.com", "a/one");
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t));
        let job = c.next_jobs(t).remove(0);
        let limited = |until| JobResult { key: Some(job.key.clone()), host_kind: Some(HostKind { kind: Some(ForgeKind::GitHub), signed_in: false }), user: Some(Err(ForgeError::RateLimited { until })), ..Default::default() };
        let note = c.finish(limited(Some(1_000_600)), t, 1_000_000);
        assert_eq!(note.as_deref(), Some("github.com: rate limited; not asking again for 10 min"));

        for reason in [Reason::Opened, Reason::AfterGit, Reason::Poll] {
            assert!(!c.request(&k, Path::new("/r"), &branches(), reason, t + Duration::from_secs(599)), "{reason:?}");
        }
        // Another answer saying the same while still resting (a job already
        // running when the limit hit) isn't logged again.
        let again = JobResult { key: Some(k.clone()), prs: Some(Err(ForgeError::RateLimited { until: None })), ..Default::default() };
        assert_eq!(c.finish(again, t + Duration::from_secs(10), 1_000_010), None);
        // Once the rest is over, everything works again.
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t + Duration::from_secs(10) + GITHUB_BACKOFF));
    }

    #[test]
    fn nearly_used_up_allowance_slows_down_and_gitlab_rests_less() {
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let k = key("gitlab.com", "g/p");
        c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t);
        let j = c.next_jobs(t).remove(0);
        let ok = JobResult {
            key: Some(j.key.clone()),
            host_kind: Some(HostKind { kind: Some(ForgeKind::GitLab), signed_in: true }),
            user: Some(Ok("tanuki".into())),
            prs: Some(Ok(BranchPrs { by_branch: HashMap::new(), slow_down_until: Some(1_000_120) })),
            ..Default::default()
        };
        assert_eq!(c.finish(ok, t, 1_000_000).as_deref(), Some("gitlab.com: nearly out of requests; not asking again for 2 min"));
        assert!(!c.request(&k, Path::new("/r"), &branches(), Reason::AfterGit, t + Duration::from_secs(119)));

        let mut c = ForgeCache::default();
        c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t);
        let j = c.next_jobs(t).remove(0);
        let hk = Some(HostKind { kind: Some(ForgeKind::GitLab), signed_in: false });
        c.finish(JobResult { key: Some(j.key), host_kind: hk, user: Some(Err(ForgeError::RateLimited { until: None })), ..Default::default() }, t, 0);
        assert!(!c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t + GITLAB_BACKOFF - Duration::from_secs(1)));
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t + GITLAB_BACKOFF));
    }

    #[test]
    fn signed_out_or_missing_cli_stops_early_and_is_checked_again_later() {
        let mut data = fake::FakeData::default();
        data.hosts.insert("github.com".into(), fake::FakeHost { kind: ForgeKind::GitHub, user: None, installed: true });
        data.hosts.insert("gitlab.com".into(), fake::FakeHost { kind: ForgeKind::GitLab, user: None, installed: false });
        let f = fake::FakeForges::new(data);
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let gh = key("github.com", "a/b");
        let gl = key("gitlab.com", "c/d");
        c.request(&gh, Path::new("/r"), &branches(), Reason::Opened, t);
        c.request(&gl, Path::new("/r"), &branches(), Reason::Opened, t);
        let done = drain(&mut c, &f, t);
        assert_eq!(done.len(), 2);
        assert_eq!(c.host("github.com").unwrap().connection, Connection::SignedOut);
        assert_eq!(c.host("gitlab.com").unwrap().connection, Connection::NotInstalled);
        assert!(c.repo(&gh).unwrap().fetched.is_none(), "no pull requests asked for");
        // Signing in elsewhere is noticed on the next focus (a minute on).
        assert!(c.request(&gh, Path::new("/r"), &branches(), Reason::Focus, t + FOCUS_EVERY));
        assert!(c.next_jobs(t + FOCUS_EVERY)[0].whoami);
    }

    #[test]
    fn unknown_hosts_are_left_alone_for_the_session() {
        let f = fake();
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let k = key("code.corp", "a/b");
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t));
        drain(&mut c, &f, t);
        assert_eq!(c.host("code.corp").unwrap().kind, None);
        assert!(!c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t + Duration::from_secs(3600)));
    }

    #[test]
    fn a_created_pull_request_shows_at_once_then_is_asked_about() {
        let f = fake();
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let k = key("github.com", "a/one");
        c.want_lists(&k, true);
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t));
        drain(&mut c, &f, t);
        let before = c.repo(&k).unwrap().lists.as_ref().unwrap().open_total;
        let new = NewPr { head: "new-thing".into(), base: "main".into(), title: "New".into(), ..Default::default() };
        let pr = created_pr(&new, "alex", "u".into(), 99, 5);
        c.add_created(&k, &pr, t, 5);
        c.add_created(&k, &pr, t, 5);
        let e = c.repo(&k).unwrap();
        assert_eq!(e.prs["new-thing"].len(), 1, "once, however often");
        let l = e.lists.as_ref().unwrap();
        assert_eq!((l.open_total, l.mine[0].number, l.open[0].number), (before + 1, 99, 99));
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::AfterPrAction, t), "and refreshed straight away");
    }

    #[test]
    fn lists_are_read_only_for_open_tabs_even_if_a_card_refresh_is_running() {
        let f = fake();
        let mut c = ForgeCache::default();
        let t = Instant::now();
        let k = key("github.com", "a/one");
        // The overview card asks first: no lists.
        assert!(c.request(&k, Path::new("/r"), &branches(), Reason::Poll, t));
        let job = c.next_jobs(t).remove(0);
        assert!(!job.lists);
        // The tab opens while that's running: asked again once it's done.
        c.want_lists(&k, true);
        assert!(!c.request(&k, Path::new("/r"), &branches(), Reason::Opened, t));
        c.finish(run_job(&f, &job), t, 7);
        assert!(c.repo(&k).unwrap().lists.is_none());
        let again = c.next_jobs(t).remove(0);
        assert!(again.lists && again.me.as_deref() == Some("alex"));
        c.finish(run_job(&f, &again), t, 8);
        let e = c.repo(&k).unwrap();
        assert_eq!((e.lists.as_ref().unwrap().open.len(), e.lists_at), (1, 8));
        assert!(c.next_jobs(t).is_empty(), "and only once");
    }

    #[test]
    fn branches_are_capped_and_deduplicated() {
        let mut c = ForgeCache::default();
        let many: Vec<String> = (0..200).map(|i| format!("b{}", i % 150)).collect();
        c.request(&key("github.com", "a/b"), Path::new("/r"), &many, Reason::Opened, Instant::now());
        let job = c.next_jobs(Instant::now()).remove(0);
        assert_eq!(job.branches.len(), MAX_BRANCHES);
        assert_eq!(job.branches[0], "b0");
    }
}
