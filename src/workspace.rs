//! Workspaces: named groups of repositories, saved between runs.

use crate::samples;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub repos: Vec<PathBuf>,
    /// Repositories open as tabs, in order.
    #[serde(default)]
    pub open_tabs: Vec<PathBuf>,
    /// `None` shows the workspace overview.
    #[serde(default)]
    pub active_tab: Option<usize>,
}

impl Workspace {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), repos: Vec::new(), open_tabs: Vec::new(), active_tab: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub workspaces: Vec<Workspace>,
    pub current: usize,
    #[serde(default)]
    pub view: ViewPrefs,
    /// Branches hidden from the history, per repository folder (full ref names).
    #[serde(default)]
    pub hidden_refs: std::collections::BTreeMap<PathBuf, Vec<String>>,
    /// Where to save; the usual settings file when empty (tests save elsewhere).
    #[serde(skip)]
    pub file: Option<PathBuf>,
}

/// How the repository screen is laid out; remembered between runs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewPrefs {
    /// The left sidebar (workspace, branches, remotes, tags, stashes) is shown.
    pub sidebar_open: bool,
    /// Share of the width (after the sidebar) given to the right-hand details pane.
    pub split: f32,
    /// Blame settings (ignore whitespace, moved lines, listed commits).
    #[serde(default)]
    pub blame: crate::git::BlameOptions,
    /// How diffs are shown.
    #[serde(default)]
    pub diff: DiffPrefs,
}

/// How diffs are shown; remembered between runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffPrefs {
    /// Changes to spaces and indentation are left out (`-w`).
    pub ignore_whitespace: bool,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self { sidebar_open: true, split: 0.5, blame: Default::default(), diff: Default::default() }
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("headway")
}

fn config_path() -> PathBuf {
    config_dir().join("workspaces.json")
}

impl Config {
    /// Load saved workspaces. On a first run this returns the sample workspace (an
    /// empty one in release builds) and whether the sample repositories still need
    /// building with [`samples::build_samples`], which is slow and belongs on a worker thread.
    pub fn load_or_init() -> (Config, bool) {
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            if let Ok(mut cfg) = serde_json::from_str::<Config>(&text) {
                if cfg.workspaces.is_empty() {
                    cfg.workspaces.push(Workspace::new("My Workspace"));
                }
                cfg.current = cfg.current.min(cfg.workspaces.len() - 1);
                let needs = samples::samples_missing(cfg.ws());
                return (cfg, needs);
            }
        }
        let cfg = Config { workspaces: vec![samples::sample_workspace()], current: 0, view: ViewPrefs::default(), hidden_refs: Default::default(), file: None };
        cfg.save();
        let needs = samples::samples_missing(cfg.ws());
        (cfg, needs)
    }

    pub fn save(&self) {
        let path = self.file.clone().unwrap_or_else(config_path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }

    /// A workspace setup for tests, saved to `file` instead of the real settings.
    #[cfg(test)]
    pub fn for_test(workspaces: Vec<Workspace>, file: PathBuf) -> Self {
        Config { workspaces, current: 0, view: ViewPrefs::default(), hidden_refs: Default::default(), file: Some(file) }
    }

    pub fn ws(&self) -> &Workspace {
        &self.workspaces[self.current]
    }

    pub fn ws_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.current]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_older_versions_still_load() {
        // Saved before layout preferences existed.
        let old = r#"{"workspaces":[{"name":"Old","repos":["/x"]}],"current":0}"#;
        let cfg: Config = serde_json::from_str(old).unwrap();
        assert_eq!(cfg.view, ViewPrefs::default());
        assert!(cfg.view.sidebar_open);
        assert_eq!(cfg.view.split, 0.5);
        let ws = cfg.ws();
        assert!(ws.open_tabs.is_empty());
        assert_eq!(ws.active_tab, None);
    }

    #[test]
    fn settings_round_trip() {
        let mut ws = Workspace::new("Client");
        ws.repos = vec!["/a".into(), "/b".into()];
        ws.open_tabs = vec!["/b".into()];
        ws.active_tab = Some(0);
        let cfg = Config { workspaces: vec![ws, Workspace::new("Other")], current: 1, view: ViewPrefs { sidebar_open: false, split: 0.3, ..Default::default() }, hidden_refs: [(PathBuf::from("/a"), vec!["refs/heads/x".to_string()])].into(), file: None };
        let back: Config = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.current, 1);
        assert_eq!(back.ws().name, "Other");
        assert_eq!(back.workspaces[0].open_tabs, [PathBuf::from("/b")]);
        assert_eq!(back.view, cfg.view);
        assert_eq!(back.hidden_refs, cfg.hidden_refs);
    }
}
