//! The command log: a record on disk of every git command Headway runs, in which
//! folder, for which user action, and how it went. It's for debugging, and for
//! clear receipts when something goes wrong in a repository.
//!
//! Not to be confused with [`crate::oplog`], the undo history: that keeps what
//! is needed to take an action back; this only records what happened.
//!
//! - One JSON object per line, in `headway-YYYY-MM-DD.jsonl` (UTC) under the log
//!   folder, written by one writer thread so git and the UI never wait on disk.
//! - Commands that change something (and every action) go to disk; read-only
//!   commands go to disk only in verbose mode, and always into a small in-memory
//!   list of recent entries that is saved if the app crashes.
//! - Logging never gets in the way: if the folder can't be written, it stops
//!   for the rest of the session without telling anyone.
//!
//! Until [`init`] runs (tests, `--screenshot`) recording does nothing. Tests use
//! their own logger per thread (see [`testing`]), which wins over the global one.

use crate::git::{self, GitResult, HeadState, UndoEntry, Undoable};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The version of the line format, on every line.
pub const SCHEMA: u32 = 1;
/// Recent entries kept in memory (read-only commands included).
pub const RECENT: usize = 500;
const STDERR_MAX: usize = 16 * 1024;
const STDOUT_MAX: usize = 4 * 1024;
const PATCH_MAX: usize = 256 * 1024;
/// Config keys whose values are never written down.
const SECRET_KEYS: [&str; 6] = ["extraheader", "token", "password", "auth", "credential", "secret"];
const TOKEN_PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "glpat-"];

// ---------------------------------------------------------------------------
// Configuration

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    /// Changes and actions on disk; read-only commands only in memory.
    Normal,
    /// Read-only commands on disk too.
    Verbose,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" => Some(Mode::Off),
            "normal" | "on" | "1" | "true" => Some(Mode::Normal),
            "verbose" => Some(Mode::Verbose),
            _ => None,
        }
    }
}

/// Where and how much to log. A plain struct, so a settings screen can fill it in.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub dir: PathBuf,
    pub mode: Mode,
    /// Files older than this many days are deleted.
    pub keep_days: u32,
    /// The folder is kept under this size, oldest files going first.
    pub max_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dir: dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("headway").join("logs"),
            mode: Mode::Normal,
            keep_days: 14,
            max_bytes: 50 * 1024 * 1024,
        }
    }
}

impl Config {
    /// The defaults (verbose in development builds), then `HEADWAY_LOG` and `HEADWAY_LOG_DIR`.
    pub fn from_env() -> Self {
        Self::default().with_env(std::env::var("HEADWAY_LOG").ok(), std::env::var_os("HEADWAY_LOG_DIR").map(PathBuf::from))
    }

    fn with_env(mut self, mode: Option<String>, dir: Option<PathBuf>) -> Self {
        if cfg!(debug_assertions) {
            self.mode = Mode::Verbose;
        }
        if let Some(m) = mode.as_deref().and_then(Mode::parse) {
            self.mode = m;
        }
        if let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
            self.dir = d;
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Reads and writes

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Read,
    Write,
}

/// The subcommand and what follows it, past git's own `-C` and `-c` options.
fn subcommand<'a>(args: &'a [&'a str]) -> Option<(&'a str, &'a [&'a str])> {
    let mut i = 0;
    while i < args.len() && matches!(args[i], "-C" | "-c") {
        i += 2;
    }
    args.get(i).map(|s| (*s, &args[i + 1..]))
}

/// Whether a git command only reads. Anything not known to be read-only counts as
/// a write, so a gap here makes the log noisier but never hides a change.
pub fn classify(args: &[&str]) -> Class {
    let Some((sub, rest)) = subcommand(args) else { return Class::Write };
    let has = |opts: &[&str]| rest.iter().any(|a| opts.contains(a));
    let first = rest.first().copied();
    let read = match sub {
        "status" | "log" | "show" | "diff" | "rev-parse" | "rev-list" | "for-each-ref" | "cat-file" | "ls-files" | "ls-tree" | "merge-base" | "blame" | "describe"
        | "check-ignore" | "grep" | "shortlog" | "version" | "var" => true,
        "hash-object" => !has(&["-w"]),
        "config" => has(&["--get", "--get-all", "--get-regexp", "--list", "-l"]),
        "branch" => {
            let writes = has(&["-d", "-D", "--delete", "-m", "-M", "--move", "-c", "-C", "--copy", "-f", "--force", "-u", "--set-upstream-to", "--unset-upstream", "--edit-description"]);
            !writes && (rest.is_empty() || has(&["--list", "-l", "-a", "--all", "-r", "--remotes", "--show-current", "--contains", "--merged"]))
        }
        "tag" => {
            let writes = has(&["-d", "--delete", "-a", "--annotate", "-s", "--sign", "-u", "-f", "--force", "-m", "-F"]);
            !writes && (rest.is_empty() || has(&["-l", "--list", "--contains", "--points-at"]) || rest.iter().any(|a| a.starts_with("-n")))
        }
        "stash" => matches!(first, Some("list" | "show")),
        "worktree" => first == Some("list"),
        "remote" => matches!(first, None | Some("-v" | "--verbose" | "get-url" | "show")),
        "submodule" => first == Some("status"),
        "symbolic-ref" => {
            // One name reads it; two set it; -d deletes it.
            let (flags, names): (Vec<&&str>, Vec<&&str>) = rest.iter().partition(|a| a.starts_with('-'));
            names.len() == 1 && flags.iter().all(|f| matches!(**f, "-q" | "--quiet" | "--short"))
        }
        "reflog" => match first {
            None => true,
            Some("show") => true,
            Some(f) => f.starts_with('-'),
        },
        "notes" => matches!(first, Some("list" | "show")),
        _ => false,
    };
    if read { Class::Read } else { Class::Write }
}

// ---------------------------------------------------------------------------
// Redaction

/// Take credentials out of text: the user part of http(s) addresses, and
/// token-shaped strings. Everything else (messages, branches, paths) is kept.
pub fn redact(s: &str) -> String {
    redact_tokens(&redact_urls(s))
}

fn redact_urls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let scheme = rest[..i].to_ascii_lowercase();
        let host_start = i + 3;
        let auth_len = rest[host_start..].find(|c: char| matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>' | '`') || c.is_whitespace()).unwrap_or(rest.len() - host_start);
        let authority = &rest[host_start..host_start + auth_len];
        out.push_str(&rest[..host_start]);
        match authority.rfind('@') {
            Some(at) if scheme.ends_with("http") || scheme.ends_with("https") => {
                out.push_str("***");
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &rest[host_start + auth_len..];
    }
    out.push_str(rest);
    out
}

fn redact_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < s.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let hit = boundary.then(|| TOKEN_PREFIXES.iter().find(|p| s[i..].starts_with(*p))).flatten();
        if let Some(p) = hit {
            let body = s[i + p.len()..].bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-').count();
            if body >= 16 {
                out.push_str("***");
                i += p.len() + body;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `key=value` of a `-c` option, with the value hidden when the key looks secret.
fn redact_config(kv: &str) -> String {
    match kv.split_once('=') {
        Some((k, _)) if SECRET_KEYS.iter().any(|s| k.to_ascii_lowercase().contains(s)) => format!("{k}=***"),
        _ => kv.to_string(),
    }
}

/// Arguments as they're written down.
pub fn redact_argv(args: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut after_c = false;
    for a in args {
        let r = redact(a);
        out.push(if after_c { redact_config(&r) } else { r });
        after_c = *a == "-c";
    }
    out
}

/// At most `max` bytes of `s` (on a character boundary), and whether it was cut.
fn cap(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Captured output as a field: redacted, cut to `max`, with its original length when cut.
fn text_field(raw: &[u8], max: usize) -> (Option<String>, Option<u64>, Option<bool>) {
    if raw.is_empty() {
        return (None, None, None);
    }
    let (text, cut) = cap(&redact(&String::from_utf8_lossy(raw)), max);
    if cut { (Some(text), Some(raw.len() as u64), Some(true)) } else { (Some(text), None, None) }
}

/// A path with the home folder written as `~`.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()) };
    }
    path.display().to_string()
}

/// FNV-1a (stable across Rust versions), as 12 hex digits.
fn short_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:012x}", h >> 16)
}

// ---------------------------------------------------------------------------
// Time, without a date library

/// RFC 3339 in UTC with milliseconds: `2026-11-05T09:30:12.345Z`.
pub fn timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (y, m, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, d.subsec_millis())
}

/// The UTC day of a moment, `YYYY-MM-DD`.
fn day_of(t: SystemTime) -> String {
    timestamp(t)[..10].to_string()
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Entries

/// A branch or tag an action moved, created (no `before`) or deleted (no `after`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefChange {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// One line of the log. Which fields are set depends on `kind`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub v: u32,
    pub ts: String,
    /// "git", "action", "session" or "panic".
    pub kind: String,
    pub session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<u64>,
    /// What the user did ("discard hunk in src/app.rs"), or "refresh".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    // git
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class: Option<Class>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    /// Environment variables Headway set for this command only, as `NAME=value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_hash: Option<String>,
    /// The patch `apply` was given, saved under `blobs/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_blob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_blob_truncated: Option<bool>,
    /// "start" or "end", for a reader that stays open (`cat-file --batch`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<u64>,
    /// The exit code, "killed", or "failed to start".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<serde_json::Value>,
    /// Why git couldn't start, or the message the user saw when an action failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_len: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_truncated: Option<bool>,

    // action
    /// The name the action has in the app ("Stage", "Discard hunk").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// The undo history entry the action left (see [`crate::oplog`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_before: Option<HeadState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_after: Option<HeadState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<RefChange>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stashes_dropped: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stashes_added: Option<Vec<(String, String)>>,
    /// How long reading the branches and tags took, before and after the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_before_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_after_ms: Option<u64>,

    // session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// The `-c` options and environment every git command gets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_config: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_env: Option<Vec<String>>,

    // panic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

// ---------------------------------------------------------------------------
// The logger and its writer thread

enum WriterMsg {
    Line { day: String, line: String, flush: bool },
    Blob { name: String, bytes: Vec<u8> },
    #[cfg_attr(not(test), allow(dead_code))]
    Flush(Sender<()>),
    Stop,
}

pub struct Logger {
    cfg: Config,
    session: String,
    tx: Sender<WriterMsg>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Set when writing failed: nothing more is logged this session.
    broken: Arc<AtomicBool>,
    recent: Mutex<VecDeque<Entry>>,
}

impl Logger {
    /// Start a logger (and its writer thread, unless logging is off).
    pub fn new(cfg: Config) -> Arc<Logger> {
        let now = SystemTime::now();
        let session = short_hash(format!("{:?}-{}", now, std::process::id()).as_bytes());
        let (tx, rx) = std::sync::mpsc::channel();
        let broken = Arc::new(AtomicBool::new(false));
        let writer = (cfg.mode != Mode::Off).then(|| {
            let start = Entry {
                v: SCHEMA,
                ts: timestamp(now),
                kind: "session".into(),
                session: session.clone(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                os: Some(std::env::consts::OS.into()),
                arch: Some(std::env::consts::ARCH.into()),
                mode: Some(cfg.mode),
                git_config: Some(git::FIXED_CONFIG.iter().map(|c| c.to_string()).collect()),
                git_env: Some(git::FIXED_ENV.iter().map(|(k, v)| format!("{k}={v}")).collect()),
                ..Default::default()
            };
            let (cfg, broken) = (cfg.clone(), broken.clone());
            std::thread::Builder::new().name("command log".into()).spawn(move || writer(cfg, start, rx, broken)).ok()
        });
        Arc::new(Logger { cfg, session, tx, writer: Mutex::new(writer.flatten()), broken, recent: Mutex::new(VecDeque::new()) })
    }

    fn active(&self) -> bool {
        self.cfg.mode != Mode::Off && !self.broken.load(Ordering::Relaxed)
    }

    fn record(&self, entry: Entry) {
        if !self.active() {
            return;
        }
        let to_disk = entry.kind != "git" || entry.class != Some(Class::Read) || self.cfg.mode == Mode::Verbose;
        if to_disk && let Ok(line) = serde_json::to_string(&entry) {
            let flush = entry.class != Some(Class::Read);
            let day = entry.ts.get(..10).map_or_else(|| day_of(SystemTime::now()), str::to_string);
            let _ = self.tx.send(WriterMsg::Line { day, line, flush });
        }
        let mut recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        if recent.len() >= RECENT {
            recent.pop_front();
        }
        recent.push_back(entry);
    }

    /// Wait until everything sent so far is on disk.
    #[cfg_attr(not(test), allow(dead_code))] // tests now; an activity view later
    pub fn flush(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.tx.send(WriterMsg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }

    /// Write what's left and stop the writer thread.
    pub fn stop(&self) {
        let _ = self.tx.send(WriterMsg::Stop);
        if let Some(h) = self.writer.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = h.join();
        }
    }

    /// The most recent entries for a repository, oldest first (this session only).
    #[cfg_attr(not(test), allow(dead_code))] // tests now; the activity view later
    pub fn recent(&self, repo: &Path, n: usize) -> Vec<Entry> {
        let repo = tilde(repo);
        let recent = self.recent.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<Entry> = recent.iter().rev().filter(|e| e.repo.as_deref() == Some(repo.as_str())).take(n).cloned().collect();
        out.reverse();
        out
    }

    fn day_file(&self, day: &str) -> PathBuf {
        self.cfg.dir.join(format!("headway-{day}.jsonl"))
    }
}

fn give_up(broken: &AtomicBool, what: &str, e: impl std::fmt::Display) {
    if !broken.swap(true, Ordering::Relaxed) && cfg!(debug_assertions) {
        eprintln!("headway: command log stopped ({what}: {e})");
    }
}

fn writer(cfg: Config, mut start: Entry, rx: Receiver<WriterMsg>, broken: Arc<AtomicBool>) {
    if let Err(e) = std::fs::create_dir_all(&cfg.dir) {
        return give_up(&broken, "can't create the folder", e);
    }
    start.git_version = git_version();
    let mut open: Option<(String, std::io::BufWriter<std::fs::File>)> = None;
    let mut rotated = String::new();
    let first = serde_json::to_string(&start).map(|line| WriterMsg::Line { day: start.ts[..10].to_string(), line, flush: true });
    for msg in first.into_iter().chain(rx.iter()) {
        match msg {
            WriterMsg::Line { day, line, flush } => {
                // At startup, and again when the day changes.
                if rotated != day {
                    rotate(&cfg.dir, cfg.keep_days, cfg.max_bytes, SystemTime::now(), Some(&format!("headway-{day}.jsonl")));
                    rotated = day.clone();
                }
                if open.as_ref().is_none_or(|(d, _)| *d != day) {
                    let file = std::fs::OpenOptions::new().create(true).append(true).open(cfg.dir.join(format!("headway-{day}.jsonl")));
                    match file {
                        Ok(f) => open = Some((day, std::io::BufWriter::new(f))),
                        Err(e) => return give_up(&broken, "can't open the log file", e),
                    }
                }
                let Some((_, w)) = open.as_mut() else { continue };
                let res = writeln!(w, "{line}").and_then(|_| if flush { w.flush() } else { Ok(()) });
                if let Err(e) = res {
                    return give_up(&broken, "can't write", e);
                }
            }
            WriterMsg::Blob { name, bytes } => {
                let dir = cfg.dir.join("blobs");
                let path = dir.join(&name);
                if !path.exists()
                    && let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &bytes))
                {
                    return give_up(&broken, "can't save a patch", e);
                }
            }
            WriterMsg::Flush(done) => {
                if let Some((_, w)) = open.as_mut() {
                    let _ = w.flush();
                }
                let _ = done.send(());
            }
            WriterMsg::Stop => break,
        }
    }
    if let Some((_, mut w)) = open {
        let _ = w.flush();
    }
}

/// `git --version`, for the session entry. Run here, not through the recorder:
/// it describes the session rather than being part of it.
fn git_version() -> Option<String> {
    let out = std::process::Command::new("git").arg("--version").stdin(std::process::Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// Whether a file in the log folder is one of ours, and so may be deleted.
fn ours(name: &str) -> bool {
    let daily = name.strip_prefix("headway-").and_then(|r| r.strip_suffix(".jsonl")).is_some_and(|d| d.len() == 10 && d.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
    let crash = name.starts_with("crash-") && name.ends_with(".jsonl");
    daily || crash
}

/// Delete our files older than `keep_days`, then the oldest until the folder is
/// under `max_bytes`. `keep` (today's file) is never deleted. Nothing else in the
/// folder is touched.
fn rotate(dir: &Path, keep_days: u32, max_bytes: u64, now: SystemTime, keep: Option<&str>) {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut add = |dir: &Path, wanted: &dyn Fn(&str) -> bool| {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !wanted(&name) || Some(name.as_str()) == keep {
                continue;
            }
            if let Ok(m) = e.metadata()
                && m.is_file()
            {
                files.push((m.modified().unwrap_or(now), m.len(), e.path()));
            }
        }
    };
    add(dir, &ours);
    add(&dir.join("blobs"), &|n: &str| n.ends_with(".patch") && n.len() == 18);
    let cutoff = now.checked_sub(Duration::from_secs(u64::from(keep_days) * 86_400)).unwrap_or(UNIX_EPOCH);
    files.retain(|(t, _, p)| !(*t < cutoff && std::fs::remove_file(p).is_ok()));
    files.sort();
    let kept = keep.and_then(|k| std::fs::metadata(dir.join(k)).ok()).map_or(0, |m| m.len());
    let mut total: u64 = kept + files.iter().map(|f| f.1).sum::<u64>();
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

// ---------------------------------------------------------------------------
// Which logger, and which action, this thread is working for

static GLOBAL: OnceLock<Arc<Logger>> = OnceLock::new();

/// The user action a thread is doing git work for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionCtx {
    pub id: Option<u64>,
    pub label: String,
}

#[derive(Clone, Default)]
struct Local {
    /// Overrides the global logger (tests).
    logger: Option<Arc<Logger>>,
    action: Option<ActionCtx>,
    /// Commands aren't recorded (the ref snapshots' own reads).
    quiet: bool,
    /// Commands that changed something since the action started.
    writes: u64,
}

thread_local! {
    static LOCAL: RefCell<Local> = RefCell::default();
}

fn current() -> Option<Arc<Logger>> {
    LOCAL.with(|l| l.borrow().logger.clone()).or_else(|| GLOBAL.get().cloned()).filter(|l| l.active())
}

/// Start logging for the app. Only the first call counts.
pub fn init(cfg: Config) {
    let _ = GLOBAL.set(Logger::new(cfg));
}

/// Write what's left and stop (when the app closes).
pub fn shutdown() {
    if let Some(l) = GLOBAL.get() {
        l.stop();
    }
}

/// Where logs go, from the environment and the defaults.
pub fn log_path() -> PathBuf {
    GLOBAL.get().map(|l| l.cfg.dir.clone()).unwrap_or_else(|| Config::from_env().dir)
}

/// The most recent entries for a repository this session, oldest first.
#[cfg_attr(not(test), allow(dead_code))] // tests now; the activity view later
pub fn recent(repo: &Path, n: usize) -> Vec<Entry> {
    LOCAL.with(|l| l.borrow().logger.clone()).or_else(|| GLOBAL.get().cloned()).map(|l| l.recent(repo, n)).unwrap_or_default()
}

/// This thread's logger and action, to carry into a worker thread.
#[must_use]
pub struct Scope(Local);

pub fn capture() -> Scope {
    LOCAL.with(|l| {
        let l = l.borrow();
        Scope(Local { logger: l.logger.clone(), action: l.action.clone(), quiet: false, writes: 0 })
    })
}

/// `f`, to run on another thread as part of this thread's action (for work that
/// starts its own threads).
pub fn carried<T>(f: impl FnOnce() -> T) -> impl FnOnce() -> T {
    let scope = capture();
    move || {
        let _scope = scope.enter();
        f()
    }
}

impl Scope {
    /// Use it on this thread until the guard is dropped.
    pub fn enter(self) -> Entered {
        Entered(Some(LOCAL.with(|l| std::mem::replace(&mut *l.borrow_mut(), self.0))))
    }
}

/// Puts back what the thread had before, when dropped.
#[must_use]
pub struct Entered(Option<Local>);

impl Drop for Entered {
    fn drop(&mut self) {
        if let Some(prev) = self.0.take() {
            LOCAL.with(|l| *l.borrow_mut() = prev);
        }
    }
}

fn change(f: impl FnOnce(&mut Local)) -> Entered {
    LOCAL.with(|l| {
        let prev = l.borrow().clone();
        f(&mut l.borrow_mut());
        Entered(Some(prev))
    })
}

/// Name the background work on this thread ("load repo", "blame"), unless it's
/// already part of an action.
pub fn named(label: &str) -> Entered {
    change(|l| {
        if l.action.is_none() {
            l.action = Some(ActionCtx { id: None, label: label.into() });
        }
    })
}

fn in_action(id: Option<u64>, label: &str) -> Entered {
    change(|l| {
        l.action = Some(ActionCtx { id, label: label.into() });
        l.writes = 0;
    })
}

fn quiet() -> Entered {
    change(|l| l.quiet = true)
}

// ---------------------------------------------------------------------------
// Recording one git command (used by crate::git)

/// What a command printed: the bytes, or only how many there were (streamed).
pub enum Out<'a> {
    Bytes(&'a [u8]),
    Len(u64),
}

/// One git command being recorded, from start to exit. Does nothing without a logger.
pub struct Call(Option<Box<(Arc<Logger>, Instant, Entry)>>);

impl Call {
    pub fn start(repo: &Path, args: &[&str], env: &[(&str, &str)], stdin: Option<&[u8]>) -> Call {
        let Some(logger) = current() else { return Call(None) };
        let class = classify(args);
        let (action, quiet) = LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            if class == Class::Write && !l.quiet {
                l.writes += 1;
            }
            (l.action.clone(), l.quiet)
        });
        if quiet {
            return Call(None);
        }
        let mut e = Entry {
            v: SCHEMA,
            ts: timestamp(SystemTime::now()),
            kind: "git".into(),
            session: logger.session.clone(),
            action_id: action.as_ref().and_then(|a| a.id),
            action: Some(action.map_or_else(|| "refresh".to_string(), |a| redact(&a.label))),
            repo: Some(tilde(repo)),
            class: Some(class),
            argv: Some(redact_argv(args)),
            env: (!env.is_empty()).then(|| env.iter().map(|(k, v)| redact_config(&format!("{k}={}", redact(v)))).collect()),
            ..Default::default()
        };
        if let Some(input) = stdin {
            let hash = short_hash(input);
            e.stdin_len = Some(input.len() as u64);
            if subcommand(args).is_some_and(|(s, _)| s == "apply") {
                let name = format!("{hash}.patch");
                let cut = input.len() > PATCH_MAX;
                let _ = logger.tx.send(WriterMsg::Blob { name: name.clone(), bytes: input[..input.len().min(PATCH_MAX)].to_vec() });
                e.stdin_blob = Some(name);
                e.stdin_blob_truncated = cut.then_some(true);
            }
            e.stdin_hash = Some(hash);
        }
        Call(Some(Box::new((logger, Instant::now(), e))))
    }

    /// For a reader that stays open: record that it started, asked for `objects` things.
    pub fn started(&mut self, objects: usize) {
        if let Some(b) = self.0.as_mut() {
            b.2.objects = Some(objects as u64);
            b.2.phase = Some("start".into());
            b.0.record(b.2.clone());
            b.2.phase = Some("end".into());
        }
    }

    /// Record how the command ended. `code`: `None` when it was killed.
    pub fn finish(self, code: Option<i32>, stdout: Out, stderr: &[u8]) {
        let Some(b) = self.0 else { return };
        let (logger, started, mut e) = *b;
        e.duration_ms = Some(ms(started.elapsed()));
        e.exit = Some(code.map_or_else(|| "killed".into(), Into::into));
        (e.stderr, e.stderr_len, e.stderr_truncated) = text_field(stderr, STDERR_MAX);
        match (stdout, e.class) {
            (Out::Bytes(b), Some(Class::Write)) => {
                (e.stdout, e.stdout_len, e.stdout_truncated) = text_field(b, STDOUT_MAX);
            }
            (Out::Bytes(b), _) => e.stdout_len = Some(b.len() as u64),
            (Out::Len(n), _) => e.stdout_len = Some(n),
        }
        logger.record(e);
    }

    /// Record that git couldn't be started (or waited for).
    pub fn failed(self, err: &std::io::Error) {
        let Some(b) = self.0 else { return };
        let (logger, started, mut e) = *b;
        e.duration_ms = Some(ms(started.elapsed()));
        e.exit = Some("failed to start".into());
        e.error = Some(err.to_string());
        logger.record(e);
    }
}

// ---------------------------------------------------------------------------
// Actions

/// HEAD, branches, tags and stashes, without logging the reads, and how long that took.
fn snapshot(repo: &Path) -> Option<(git::RefState, u64)> {
    if !git::is_repo(repo) {
        return None;
    }
    let _quiet = quiet();
    let t = Instant::now();
    git::ref_state(repo).ok().map(|s| (s, ms(t.elapsed())))
}

/// Run a user action's git work, linking every command to it, then record one
/// "action" entry: how it went, the message the user saw if it failed
/// (`failure`), the undo entry it left, and the branches and tags it moved.
///
/// `record` makes it undoable (through [`crate::oplog::run_recorded`]), and its
/// text becomes the action's name.
pub fn run_action(
    repo: &Path,
    id: u64,
    label: &str,
    record: Option<(&Undoable, &str)>,
    op: impl FnOnce(&Path) -> GitResult<String>,
    failure: impl FnOnce(&str) -> String,
) -> (GitResult<String>, Option<UndoEntry>) {
    let what = record.map_or(label, |r| r.1);
    traced(repo, id, what, label, record.is_none(), failure, || match record {
        Some((undo, what)) => match crate::oplog::run_recorded(repo, undo, what, op) {
            Ok((out, entry)) => (Ok(out), entry),
            Err(e) => (Err(e), None),
        },
        None => (op(repo), None),
    })
}

/// Like [`run_action`] for any work. `snapshot`: compare the branches and tags
/// before and after (when it isn't undoable, and there's a repository already).
pub fn traced<T>(
    repo: &Path,
    id: u64,
    what: &str,
    label: &str,
    snapshot_refs: bool,
    failure: impl FnOnce(&str) -> String,
    work: impl FnOnce() -> (GitResult<T>, Option<UndoEntry>),
) -> (GitResult<T>, Option<UndoEntry>) {
    let _action = in_action(Some(id), what);
    let logger = current();
    let started = Instant::now();
    let before = if logger.is_some() && snapshot_refs { snapshot(repo) } else { None };
    let (result, undo) = work();
    let Some(logger) = logger else { return (result, undo) };
    let wrote = LOCAL.with(|l| l.borrow().writes) > 0;
    let mut e = Entry {
        v: SCHEMA,
        ts: timestamp(SystemTime::now()),
        kind: "action".into(),
        session: logger.session.clone(),
        action_id: Some(id),
        action: Some(redact(what)),
        label: Some(redact(label)),
        repo: Some(tilde(repo)),
        ok: Some(result.is_ok()),
        error: result.as_ref().err().map(|err| redact(&failure(err))),
        undo_id: undo.as_ref().map(|u| u.id),
        ..Default::default()
    };
    if let Some(u) = &undo {
        e.head_before = Some(u.head_before.clone());
        e.head_after = Some(u.head_after.clone());
        e.refs = Some(u.refs.iter().map(|r| RefChange { name: r.name.clone(), before: r.before.clone(), after: r.after.clone() }).collect()).filter(|v: &Vec<_>| !v.is_empty());
        e.stashes_dropped = Some(u.dropped_stashes.clone()).filter(|v| !v.is_empty());
    }
    if let Some((before, took)) = before {
        e.snapshot_before_ms = Some(took);
        if wrote && let Some((after, took)) = snapshot(repo) {
            e.snapshot_after_ms = Some(took);
            describe_changes(&mut e, &before, &after);
        }
    }
    e.duration_ms = Some(ms(started.elapsed()));
    logger.record(e);
    (result, undo)
}

/// HEAD before and after, and only the refs and stashes that changed.
fn describe_changes(e: &mut Entry, before: &git::RefState, after: &git::RefState) {
    e.head_before = Some(before.head.clone());
    e.head_after = Some(after.head.clone());
    let mut names: Vec<&String> = before.refs.keys().chain(after.refs.keys()).collect();
    names.sort();
    names.dedup();
    let refs: Vec<RefChange> = names
        .into_iter()
        .filter(|n| before.refs.get(*n) != after.refs.get(*n))
        .map(|n| RefChange { name: n.clone(), before: before.refs.get(n).cloned(), after: after.refs.get(n).cloned() })
        .collect();
    let missing = |a: &[(String, String)], b: &[(String, String)]| a.iter().filter(|s| !b.iter().any(|t| t.0 == s.0)).cloned().collect::<Vec<_>>();
    e.refs = Some(refs).filter(|v| !v.is_empty());
    e.stashes_dropped = Some(missing(&before.stashes, &after.stashes)).filter(|v| !v.is_empty());
    e.stashes_added = Some(missing(&after.stashes, &before.stashes)).filter(|v| !v.is_empty());
}

// ---------------------------------------------------------------------------
// Crashes

/// Record panics: a "panic" entry, and the recent entries in a crash file. Both
/// are written right here, since the writer thread may not run again. Then the
/// panic goes on to the hook that was there before.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(logger) = GLOBAL.get().filter(|l| l.active()) {
            let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned());
            write_crash(logger, message.unwrap_or_else(|| "panic".into()), info.location().map(|l| l.to_string()));
        }
        previous(info);
    }));
}

fn write_crash(logger: &Logger, message: String, location: Option<String>) {
    let now = SystemTime::now();
    let entry = Entry {
        v: SCHEMA,
        ts: timestamp(now),
        kind: "panic".into(),
        session: logger.session.clone(),
        message: Some(redact(&message)),
        location,
        ..Default::default()
    };
    let append = |path: PathBuf, lines: &[String]| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(lines.iter().map(|l| format!("{l}\n")).collect::<String>().as_bytes())
    };
    if let Ok(line) = serde_json::to_string(&entry) {
        let _ = std::fs::create_dir_all(&logger.cfg.dir);
        let _ = append(logger.day_file(&day_of(now)), std::slice::from_ref(&line));
        // The panic may have happened while the list was being changed.
        let recent: Vec<String> = match logger.recent.try_lock() {
            Ok(r) => r.iter().filter_map(|e| serde_json::to_string(e).ok()).collect(),
            Err(_) => Vec::new(),
        };
        let stamp: String = entry.ts.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let _ = append(logger.cfg.dir.join(format!("crash-{stamp}.jsonl")), &[std::slice::from_ref(&line), &recent[..]].concat());
    }
}

// ---------------------------------------------------------------------------
// Export

/// Write the last `days` days of the log, and every crash file, into one file to
/// attach to a bug report, with the home folder written as `~`.
pub fn export(out: &Path, days: u32) -> std::io::Result<usize> {
    let home = dirs::home_dir().map(|h| h.display().to_string());
    export_from(&log_path(), out, days, SystemTime::now(), home.as_deref())
}

fn export_from(dir: &Path, out: &Path, days: u32, now: SystemTime, home: Option<&str>) -> std::io::Result<usize> {
    let first_day = day_of(now.checked_sub(Duration::from_secs(u64::from(days.max(1) - 1) * 86_400)).unwrap_or(UNIX_EPOCH));
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| match n.strip_prefix("headway-").and_then(|r| r.strip_suffix(".jsonl")) {
            Some(day) => ours(n) && day >= first_day.as_str(),
            None => n.starts_with("crash-") && n.ends_with(".jsonl"),
        })
        .collect();
    // Daily files first, by day, then crash files.
    names.sort_by_key(|n| (n.starts_with("crash-"), n.clone()));
    let mut text = String::new();
    for n in &names {
        let s = std::fs::read_to_string(dir.join(n))?;
        text.push_str(&s);
        if !s.is_empty() && !s.ends_with('\n') {
            text.push('\n');
        }
    }
    if let Some(home) = home.filter(|h| h.len() > 1) {
        // As written in JSON (backslashes doubled on Windows), and as is.
        let escaped = serde_json::to_string(home).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        text = text.replace(escaped, "~").replace(home, "~");
    }
    std::fs::write(out, &text)?;
    Ok(text.lines().count())
}

// ---------------------------------------------------------------------------
// Tests

/// A logger of its own for one test, in its own temporary folder, used by this
/// thread (and the worker threads it starts through the app) instead of the
/// global one. Tests run in parallel, so they can't share a folder.
#[cfg(test)]
pub mod testing {
    use super::*;

    pub struct TestLog {
        pub logger: Arc<Logger>,
        pub dir: PathBuf,
        _entered: Entered,
    }

    impl TestLog {
        pub fn new(name: &str, mode: Mode) -> TestLog {
            let dir = std::env::temp_dir().join(format!("headway-cmdlog-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let logger = Logger::new(Config { dir: dir.clone(), mode, ..Config::default() });
            let entered = change(|l| l.logger = Some(logger.clone()));
            TestLog { logger, dir, _entered: entered }
        }

        /// Everything on disk so far (each daily file), oldest first.
        pub fn on_disk(&self) -> Vec<Entry> {
            self.logger.flush();
            let mut names: Vec<PathBuf> = std::fs::read_dir(&self.dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
            names.sort();
            names.iter().flat_map(|p| std::fs::read_to_string(p).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect::<Vec<Entry>>()).collect()
        }

        pub fn recent(&self, repo: &Path) -> Vec<Entry> {
            self.logger.recent(repo, RECENT)
        }
    }

    impl Drop for TestLog {
        fn drop(&mut self) {
            self.logger.stop();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TestLog;
    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("headway-test-cmdlog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "T"], &["config", "user.email", "t@example.com"], &["config", "commit.gpgsign", "false"]] {
            git::run(&dir, args).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "1\n").unwrap();
        git::run(&dir, &["add", "a.txt"]).unwrap();
        git::run(&dir, &["commit", "-q", "-m", "1"]).unwrap();
        dir
    }

    #[test]
    fn read_only_commands_are_told_apart_from_changes() {
        use Class::*;
        let table: &[(&[&str], Class)] = &[
            (&["status", "--porcelain=v1"], Read),
            (&["log", "-1"], Read),
            (&["show", "HEAD"], Read),
            (&["diff", "--cached"], Read),
            (&["rev-parse", "HEAD"], Read),
            (&["rev-list", "--count", "HEAD"], Read),
            (&["for-each-ref"], Read),
            (&["cat-file", "--batch"], Read),
            (&["ls-files", "-z"], Read),
            (&["ls-tree", "HEAD"], Read),
            (&["merge-base", "a", "b"], Read),
            (&["blame", "--porcelain", "f"], Read),
            (&["describe", "--tags"], Read),
            (&["check-ignore", "x"], Read),
            (&["grep", "foo"], Read),
            (&["shortlog", "-s"], Read),
            (&["version"], Read),
            (&["var", "GIT_EDITOR"], Read),
            (&["hash-object", "--", "a"], Read),
            (&["hash-object", "-w", "--stdin"], Write),
            (&["config", "--get", "merge.tool"], Read),
            (&["config", "--get-all", "x"], Read),
            (&["config", "-f", ".gitmodules", "--get-regexp", "^sub"], Read),
            (&["config", "--list"], Read),
            (&["config", "user.name", "Ana"], Write),
            (&["branch", "--list"], Read),
            (&["branch", "-a"], Read),
            (&["branch", "-r"], Read),
            (&["branch", "--show-current"], Read),
            (&["branch", "--contains", "abc"], Read),
            (&["branch", "--merged"], Read),
            (&["branch", "topic"], Write),
            (&["branch", "-D", "topic"], Write),
            (&["branch", "-r", "-d", "origin/x"], Write),
            (&["tag", "-l"], Read),
            (&["tag", "--list"], Read),
            (&["tag", "-n"], Read),
            (&["tag", "--contains", "abc"], Read),
            (&["tag", "--points-at", "HEAD"], Read),
            (&["tag", "v1"], Write),
            (&["tag", "-d", "v1"], Write),
            (&["stash", "list"], Read),
            (&["stash", "show", "-p"], Read),
            (&["stash", "push"], Write),
            (&["stash"], Write),
            (&["worktree", "list", "--porcelain"], Read),
            (&["worktree", "add", "x"], Write),
            (&["remote"], Read),
            (&["remote", "-v"], Read),
            (&["remote", "get-url", "origin"], Read),
            (&["remote", "show", "origin"], Read),
            (&["remote", "add", "o", "u"], Write),
            (&["submodule", "status"], Read),
            (&["submodule", "update", "--init"], Write),
            (&["symbolic-ref", "HEAD"], Read),
            (&["symbolic-ref", "-q", "HEAD"], Read),
            (&["symbolic-ref", "-q", "--short", "HEAD"], Read),
            (&["symbolic-ref", "HEAD", "refs/heads/x"], Write),
            (&["symbolic-ref", "-d", "HEAD"], Write),
            (&["reflog"], Read),
            (&["reflog", "show", "main"], Read),
            (&["reflog", "-1"], Read),
            (&["reflog", "expire", "--all"], Write),
            (&["reflog", "delete", "HEAD@{1}"], Write),
            (&["notes", "list"], Read),
            (&["notes", "show"], Read),
            (&["notes", "add", "-m", "x"], Write),
            (&["commit", "-m", "x"], Write),
            (&["apply", "--cached"], Write),
            (&["fetch"], Write),
            (&["frobnicate"], Write),
            (&[], Write),
            // git's own options in front are skipped.
            (&["-c", "core.x=1", "-C", "dir", "status"], Read),
            (&["-c", "core.x=1", "commit"], Write),
        ];
        for (args, want) in table {
            assert_eq!(classify(args), *want, "{args:?}");
        }
    }

    #[test]
    fn credentials_are_taken_out() {
        assert_eq!(redact("https://ana:ghx_secret@github.com/org/repo.git"), "https://***@github.com/org/repo.git");
        assert_eq!(redact("HTTP://token@example.com"), "HTTP://***@example.com");
        assert_eq!(redact("fatal: unable to access 'https://ana:pw@host/x/': 403"), "fatal: unable to access 'https://***@host/x/': 403");
        // No user part, or ssh: unchanged (the user name there isn't a secret).
        assert_eq!(redact("https://github.com/org/repo"), "https://github.com/org/repo");
        assert_eq!(redact("git@github.com:org/repo.git"), "git@github.com:org/repo.git");
        assert_eq!(redact("ssh://git@host:22/org/repo"), "ssh://git@host:22/org/repo");
        // -c options with secret-looking keys.
        assert_eq!(
            redact_argv(&["-c", "http.extraHeader=AUTHORIZATION: bearer abc", "-c", "core.editor=vim", "fetch"]),
            ["-c", "http.extraHeader=***", "-c", "core.editor=vim", "fetch"]
        );
        assert_eq!(redact_argv(&["-c", "credential.helper=store", "-c", "my.Password=x"]), ["-c", "credential.helper=***", "-c", "my.Password=***"]);
        assert_eq!(redact_argv(&["clone", "https://u:p@h/r", "r"]), ["clone", "https://***@h/r", "r"]);
        // Token-shaped strings anywhere.
        for t in ["ghp_", "gho_", "ghu_", "ghs_", "github_pat_", "glpat-"] {
            let token = format!("{t}A1b2C3d4E5f6G7h8J9k0");
            assert_eq!(redact(&format!("remote: bad token {token}.\n")), "remote: bad token ***.\n", "{t}");
        }
        // Too short, or part of a word: left alone. So are messages, branches and paths.
        assert_eq!(redact("see ghp_short and xghp_A1b2C3d4E5f6G7h8J9k0"), "see ghp_short and xghp_A1b2C3d4E5f6G7h8J9k0");
        assert_eq!(redact_argv(&["commit", "-m", "Fix the login page", "--", "src/auth.rs"]), ["commit", "-m", "Fix the login page", "--", "src/auth.rs"]);
    }

    #[test]
    fn long_output_is_cut_and_says_so() {
        assert_eq!(text_field(b"", 10), (None, None, None));
        assert_eq!(text_field(b"short", 10), (Some("short".into()), None, None));
        assert_eq!(text_field(b"0123456789abc", 10), (Some("0123456789".into()), Some(13), Some(true)));
        // Never in the middle of a character.
        let (text, len, cut) = text_field("ééééé".as_bytes(), 5);
        assert_eq!((text.as_deref(), len, cut), (Some("éé"), Some(10), Some(true)));
        let json = serde_json::to_string(&Entry { stderr: Some("x".into()), stderr_len: Some(20_000), stderr_truncated: Some(true), ..Default::default() }).unwrap();
        assert!(json.contains("\"stderr_truncated\":true") && json.contains("\"stderr_len\":20000"), "{json}");
        assert!(!json.contains("stdout"), "unset fields are left out: {json}");
    }

    #[test]
    fn times_are_utc_with_milliseconds() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_millis(1_709_251_199_123)), "2024-02-29T23:59:59.123Z");
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(4_102_444_800)), "2100-01-01T00:00:00.000Z");
    }

    #[test]
    fn the_environment_picks_the_mode_and_folder() {
        let d = Config::default();
        assert_eq!((d.keep_days, d.max_bytes, d.mode), (14, 50 * 1024 * 1024, Mode::Normal));
        assert!(d.dir.ends_with(Path::new("headway").join("logs")));
        let dev = if cfg!(debug_assertions) { Mode::Verbose } else { Mode::Normal };
        assert_eq!(Config::default().with_env(None, None).mode, dev);
        assert_eq!(Config::default().with_env(Some("off".into()), None).mode, Mode::Off);
        assert_eq!(Config::default().with_env(Some("Verbose".into()), None).mode, Mode::Verbose);
        assert_eq!(Config::default().with_env(Some("nonsense".into()), None).mode, dev);
        assert_eq!(Config::default().with_env(None, Some("/x/logs".into())).dir, PathBuf::from("/x/logs"));
    }

    fn git_entries(entries: &[Entry]) -> Vec<&Entry> {
        entries.iter().filter(|e| e.kind == "git").collect()
    }

    #[test]
    fn an_undoable_action_takes_its_refs_from_the_undo_entry() {
        let dir = temp_repo("undoable");
        let old = git::run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        git::stage(&dir, &["a.txt".into()]).unwrap();
        let log = TestLog::new("undoable", Mode::Normal);
        let (res, undo) = run_action(&dir, 7, "Commit", Some((&Undoable::Refs, "commit “2”")), |d| git::commit(d, "2", false), |e| e.to_string());
        res.unwrap();
        let new = git::run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let recent = log.recent(&dir);
        let action = recent.iter().find(|e| e.kind == "action").unwrap();
        assert_eq!(action.undo_id, Some(undo.unwrap().id));
        assert_eq!((action.action.as_deref(), action.label.as_deref(), action.ok), (Some("commit “2”"), Some("Commit"), Some(true)));
        assert_eq!(action.refs.as_ref().unwrap(), &[RefChange { name: "refs/heads/main".into(), before: Some(old.clone()), after: Some(new.clone()) }]);
        assert_eq!(action.head_before.as_ref().unwrap().oid.as_deref(), Some(old.as_str()));
        assert_eq!(action.head_after.as_ref().unwrap().oid.as_deref(), Some(new.as_str()));
        assert!(action.snapshot_before_ms.is_none(), "no second snapshot");
        // Only the undo history's own two reads of the refs (before and after), nothing more.
        let ref_reads = recent.iter().filter(|e| e.argv.as_ref().is_some_and(|a| a[0] == "for-each-ref" && a.contains(&"refs/heads".to_string()))).count();
        assert_eq!(ref_reads, 2);
        assert!(log.on_disk().iter().any(|e| e.kind == "action" && e.undo_id.is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_actions_snapshot_refs_only_when_something_was_written() {
        let dir = temp_repo("snapshots");
        let log = TestLog::new("snapshots", Mode::Normal);
        std::fs::write(dir.join("a.txt"), "2\n").unwrap();
        let (res, _) = run_action(&dir, 1, "Stage", None, |d| git::stage(d, &["a.txt".into()]), |e| e.to_string());
        res.unwrap();
        let (res, _) = run_action(&dir, 2, "Look", None, |d| git::run(d, &["status"]), |e| e.to_string());
        res.unwrap();
        let (res, _) = run_action(&dir, 3, "Branch", None, |d| git::run(d, &["branch", "topic"]), |e| e.to_string());
        res.unwrap();
        let recent = log.recent(&dir);
        let action = |id| recent.iter().find(|e| e.kind == "action" && e.action_id == Some(id)).unwrap();
        let stage = action(1);
        assert!(stage.snapshot_before_ms.is_some() && stage.snapshot_after_ms.is_some());
        assert_eq!(stage.head_before, stage.head_after);
        assert!(stage.refs.is_none(), "no ref changed");
        let look = action(2);
        assert!(look.snapshot_after_ms.is_none() && look.head_before.is_none(), "a read-only action gets no comparison");
        let branch = action(3);
        let head = git::run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_eq!(branch.refs.as_ref().unwrap(), &[RefChange { name: "refs/heads/topic".into(), before: None, after: Some(head) }]);
        // The snapshots' own reads aren't in the log.
        assert!(!recent.iter().any(|e| e.argv.as_ref().is_some_and(|a| a[0] == "for-each-ref")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_command_names_its_action_and_failures_keep_the_message_shown() {
        let dir = temp_repo("ids");
        let log = TestLog::new("ids", Mode::Verbose);
        let (res, _) = run_action(
            &dir,
            42,
            "Check out nowhere",
            None,
            |d| {
                git::run(d, &["status"])?;
                git::run(d, &["checkout", "nowhere"])
            },
            |e| format!("Check out nowhere failed\n{e}"),
        );
        let err = res.unwrap_err();
        let disk = log.on_disk();
        let git = git_entries(&disk);
        assert_eq!(git.len(), 2);
        assert!(git.iter().all(|e| e.action_id == Some(42) && e.action.as_deref() == Some("Check out nowhere")));
        let action = disk.iter().find(|e| e.kind == "action").unwrap();
        assert_eq!(action.ok, Some(false));
        assert_eq!(action.error.as_deref(), Some(format!("Check out nowhere failed\n{err}").as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_work_is_named_and_carried_into_threads() {
        let dir = temp_repo("named");
        let log = TestLog::new("named", Mode::Normal);
        let scope = capture();
        std::thread::spawn({
            let dir = dir.clone();
            move || {
                let _s = scope.enter();
                let _n = named("load repo");
                git::run(&dir, &["status"]).unwrap();
            }
        })
        .join()
        .unwrap();
        let recent = log.recent(&dir);
        assert_eq!(recent.len(), 1, "recorded by the test's logger, from another thread");
        assert_eq!((recent[0].action.as_deref(), recent[0].action_id), (Some("load repo"), None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn touch(path: &Path, bytes: usize, days_old: u64, now: SystemTime) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(now - Duration::from_secs(days_old * 86_400 + 60)).unwrap();
    }

    #[test]
    fn rotation_removes_old_and_extra_files_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("headway-cmdlog-rotate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, undo) = (root.join("logs"), root.join("undo"));
        let now = SystemTime::now();
        touch(&dir.join("headway-2020-01-01.jsonl"), 10, 30, now);
        touch(&dir.join("crash-20200101T000000000Z.jsonl"), 10, 30, now);
        touch(&dir.join("blobs").join("0123456789ab.patch"), 10, 30, now);
        touch(&dir.join("notes.txt"), 10, 30, now);
        touch(&dir.join("headway-notes.jsonl"), 10, 30, now);
        touch(&undo.join("web-0000000000000000.json"), 10, 30, now);
        touch(&dir.join("headway-2026-01-03.jsonl"), 600, 3, now);
        touch(&dir.join("headway-2026-01-04.jsonl"), 600, 2, now);
        touch(&dir.join("headway-2026-01-05.jsonl"), 600, 0, now);
        rotate(&dir, 14, 1000, now, Some("headway-2026-01-05.jsonl"));
        let left = |d: &Path| {
            let mut v: Vec<String> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        // Old ones gone; then the oldest recent ones until it fits; today's stays.
        assert_eq!(left(&dir), ["blobs", "headway-2026-01-05.jsonl", "headway-notes.jsonl", "notes.txt"]);
        assert!(left(&dir.join("blobs")).is_empty());
        assert_eq!(left(&undo), ["web-0000000000000000.json"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_export_has_recent_days_and_crashes_with_home_written_as_tilde() {
        let root = std::env::temp_dir().join(format!("headway-cmdlog-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_762_300_800); // 2025-11-05
        let line = |day: &str, path: &str| serde_json::to_string(&Entry { ts: format!("{day}T00:00:00.000Z"), repo: Some(path.into()), ..Default::default() }).unwrap() + "\n";
        std::fs::write(dir.join("headway-2025-11-01.jsonl"), line("2025-11-01", "/home/ana/old")).unwrap();
        std::fs::write(dir.join("headway-2025-11-04.jsonl"), line("2025-11-04", "/home/ana/web")).unwrap();
        std::fs::write(dir.join("headway-2025-11-05.jsonl"), line("2025-11-05", r"C:\Users\ana\web")).unwrap();
        std::fs::write(dir.join("crash-20251101T000000000Z.jsonl"), line("2025-11-01", "/home/ana/crash")).unwrap();
        let out = root.join("export.jsonl");
        assert_eq!(export_from(&dir, &out, 2, now, Some("/home/ana")).unwrap(), 3);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("\"~/web\"") && text.contains("\"~/crash\"") && !text.contains("old"), "{text}");
        assert!(!text.contains("/home/ana"));
        // Windows paths are written in JSON with doubled backslashes.
        export_from(&dir, &out, 2, now, Some(r"C:\Users\ana")).unwrap();
        assert!(std::fs::read_to_string(&out).unwrap().contains(r#""~\\web""#));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_crash_writes_the_panic_and_recent_reads() {
        let dir = temp_repo("crash");
        let log = TestLog::new("crash", Mode::Normal);
        git::run(&dir, &["status"]).unwrap();
        write_crash(&log.logger, "boom with ghp_A1b2C3d4E5f6G7h8J9k0".into(), Some("src/app.rs:1:1".into()));
        let crash = std::fs::read_dir(&log.dir).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("crash-")).unwrap().path();
        let lines: Vec<Entry> = std::fs::read_to_string(crash).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[0].kind, "panic");
        assert_eq!(lines[0].message.as_deref(), Some("boom with ***"));
        assert!(lines[1..].iter().any(|e| e.argv.as_ref().is_some_and(|a| a[0] == "status")));
        assert!(log.on_disk().iter().any(|e| e.kind == "panic"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
