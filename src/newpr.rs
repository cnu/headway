//! The "Create pull request" dialog ("merge request" on GitLab): what it holds,
//! how it fills itself in, and how it's drawn. The app does the work around it:
//! comparing with the base (git, on a worker), listing reviewers and labels,
//! pushing and creating (one action, in the command log).

use crate::forge::{ForgeKind, NewPr, PrMeta, RepoKey};
use crate::git::{self, PrPrep};
use crate::theme::*;
use egui::{Align, Layout, RichText, Ui};
use std::path::PathBuf;
use std::sync::Arc;

pub struct PrDraft {
    pub repo: PathBuf,
    pub kind: ForgeKind,
    pub key: RepoKey,
    /// The signed-in user (the author).
    pub me: String,
    /// The local branch.
    pub branch: String,
    /// The remote the pull request goes to (`upstream` for a fork), and the one
    /// its branch is pushed to (`origin`).
    pub base_remote: String,
    pub push_remote: String,
    /// The fork the branch is in (`owner/repo`), when it isn't the project itself.
    pub head_repo: Option<String>,
    /// Empty until known (the forge's default branch, or the remote's HEAD).
    pub base: String,
    /// The comparison with `base`, once worked out (for `generation`).
    pub prep: Option<Result<PrPrep, String>>,
    pub generation: u64,
    pub title: String,
    /// What was filled in, to tell whether the person has typed over it.
    auto_title: String,
    pub body: String,
    auto_body: String,
    pub template: Option<usize>,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    pub remove_source_branch: bool,
    pub squash: bool,
    /// People and labels to pick from (`None` while loading).
    pub meta: Option<Result<Arc<PrMeta>, String>>,
    /// Being pushed and created; the dialog waits.
    pub creating: bool,
    /// Creating failed, with this.
    pub error: Option<String>,
}

/// What the dialog wants done after a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Keep,
    Close,
    /// The base was changed: compare again.
    BaseChanged,
    Create,
}

impl PrDraft {
    #[allow(clippy::too_many_arguments)]
    pub fn new(repo: PathBuf, kind: ForgeKind, key: RepoKey, me: String, branch: String, base_remote: String, push_remote: String, head_repo: Option<String>, base: String) -> Self {
        PrDraft {
            repo,
            kind,
            key,
            me,
            branch,
            base_remote,
            push_remote,
            head_repo,
            base,
            prep: None,
            generation: 0,
            title: String::new(),
            auto_title: String::new(),
            body: String::new(),
            auto_body: String::new(),
            template: None,
            draft: false,
            reviewers: Vec::new(),
            assignees: Vec::new(),
            labels: Vec::new(),
            // GitLab's own default for new merge requests.
            remove_source_branch: kind == ForgeKind::GitLab,
            squash: false,
            meta: None,
            creating: false,
            error: None,
        }
    }

    /// "pull request" or "merge request".
    pub fn noun(&self) -> &'static str {
        match self.kind {
            ForgeKind::GitHub => "pull request",
            ForgeKind::GitLab => "merge request",
        }
    }

    /// The comparison for `generation` came back: fill in the title and the
    /// description, unless they've been typed over.
    pub fn prepared(&mut self, generation: u64, result: Result<PrPrep, String>) {
        if generation != self.generation {
            return;
        }
        if let Ok(p) = &result {
            let title = git::pr_title(&p.subjects, &self.branch);
            if self.title.trim().is_empty() || self.title == self.auto_title {
                self.title = title.clone();
            }
            self.auto_title = title;
            if self.template.is_none_or(|i| i >= p.templates.len()) {
                self.template = (!p.templates.is_empty()).then_some(0);
            }
            let body = match self.template {
                Some(i) => p.templates[i].body.clone(),
                None => git::pr_body_from_commits(&p.subjects),
            };
            self.set_body(body);
        }
        self.prep = Some(result);
    }

    /// Fill in the description, unless it's been typed over.
    fn set_body(&mut self, body: String) {
        if self.body.trim().is_empty() || self.body == self.auto_body {
            self.body = body.clone();
        }
        self.auto_body = body;
    }

    fn ready(&self) -> Option<&PrPrep> {
        self.prep.as_ref()?.as_ref().ok()
    }

    pub fn can_create(&self) -> bool {
        !self.creating && !self.title.trim().is_empty() && self.ready().is_some_and(|p| !p.subjects.is_empty())
    }

    /// "3 commits, 7 files changed, into upstream/main".
    pub fn summary(&self) -> Option<String> {
        let p = self.ready()?;
        let (c, f) = (p.subjects.len(), p.files);
        Some(format!("{c} commit{}, {f} file{} changed, into {}/{}", s(c), s(f), self.base_remote, self.base))
    }

    /// "This will conflict with main in 2 files", with the files.
    pub fn conflict_warning(&self) -> Option<(String, &[String])> {
        let files = self.ready()?.conflicts.as_deref().filter(|f| !f.is_empty())?;
        let n = files.len();
        Some((format!("This will conflict with {} in {n} file{}", self.base, s(n)), files))
    }

    /// What to send to the forge.
    pub fn new_pr(&self) -> Option<NewPr> {
        let p = self.ready()?;
        let gitlab = self.kind == ForgeKind::GitLab;
        Some(NewPr {
            head: p.head.clone(),
            head_repo: self.head_repo.clone(),
            base: self.base.clone(),
            title: self.title.trim().to_string(),
            body: self.body.trim_end().to_string(),
            draft: self.draft,
            reviewers: self.reviewers.clone(),
            assignees: self.assignees.clone(),
            labels: self.labels.clone(),
            remove_source_branch: gitlab && self.remove_source_branch,
            squash: gitlab && self.squash,
        })
    }
}

fn s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Draw the dialog's contents.
pub fn ui(ui: &mut Ui, d: &mut PrDraft) -> Outcome {
    let mut out = Outcome::Keep;
    let noun = d.noun();
    ui.set_width(560.0);
    ui.heading(format!("Create {noun}"));
    let from = match &d.head_repo {
        Some(fork) => format!("From {}:{}", fork.split('/').next().unwrap_or(fork), d.branch),
        None => format!("From {}", d.branch),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(from).color(DIM));
        ui.label(RichText::new(format!("into {}/", d.base_remote)).color(DIM));
        let bases: Vec<String> = match d.ready() {
            Some(p) if !p.bases.is_empty() => p.bases.clone(),
            _ => vec![d.base.clone()],
        };
        let shown = if d.base.is_empty() { "…".to_string() } else { d.base.clone() };
        let combo = egui::ComboBox::from_id_salt("pr-base").selected_text(shown).height(300.0).show_ui(ui, |ui| {
            for b in &bases {
                if ui.selectable_label(*b == d.base, b).clicked() && *b != d.base {
                    d.base = b.clone();
                    out = Outcome::BaseChanged;
                }
            }
        });
        combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Base branch"));
    });
    ui.add_space(8.0);

    ui.label("Title");
    ui.add(egui::TextEdit::singleline(&mut d.title).desired_width(f32::INFINITY));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Description");
        let names: Vec<(String, String)> = d.ready().map(|p| p.templates.iter().map(|t| (t.name().to_string(), t.body.clone())).collect()).unwrap_or_default();
        if names.len() > 1 {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let current = d.template.and_then(|i| names.get(i)).map_or("None", |t| t.0.as_str()).to_string();
                let mut picked = None;
                let combo = egui::ComboBox::from_id_salt("pr-template").selected_text(current).show_ui(ui, |ui| {
                    for (i, (name, _)) in names.iter().enumerate() {
                        if ui.selectable_label(d.template == Some(i), name).clicked() {
                            picked = Some(i);
                        }
                    }
                });
                combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Template"));
                ui.label(RichText::new("Template").color(DIM));
                if let Some(i) = picked {
                    // Picking one is asking for it: it replaces what's there.
                    d.template = Some(i);
                    d.body = names[i].1.clone();
                    d.auto_body = d.body.clone();
                }
            });
        }
    });
    egui::ScrollArea::vertical().id_salt("pr-body").max_height(200.0).show(ui, |ui| {
        ui.add(egui::TextEdit::multiline(&mut d.body).desired_rows(8).desired_width(f32::INFINITY));
    });
    ui.add_space(6.0);

    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut d.draft, "Draft").on_hover_text("Open it as a draft: not ready for review or merging yet");
        if d.kind == ForgeKind::GitLab {
            ui.checkbox(&mut d.remove_source_branch, "Delete source branch when merged");
            ui.checkbox(&mut d.squash, "Squash commits when merged");
        }
    });
    ui.add_space(4.0);
    match &d.meta {
        None => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(ACCENT));
                ui.label(RichText::new("Loading reviewers and labels…").small().color(DIM));
            });
        }
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Couldn't list reviewers and labels: {e}")).small().color(WARN));
        }
        Some(Ok(meta)) => {
            let meta = meta.clone();
            egui::Grid::new("pr-people").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                picker(ui, "Reviewers", &meta.users, &mut d.reviewers);
                picker(ui, "Assignees", &meta.users, &mut d.assignees);
                picker(ui, "Labels", &meta.labels, &mut d.labels);
            });
        }
    }
    ui.add_space(8.0);

    // Where it stands: what goes in, what's pushed first, what will conflict.
    match &d.prep {
        None => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(ACCENT));
                ui.label(RichText::new(format!("Comparing with {}…", if d.base.is_empty() { "the base branch" } else { &d.base })).color(DIM));
            });
        }
        Some(Err(e)) => {
            ui.add(egui::Label::new(RichText::new(e).color(WARN)).wrap());
        }
        Some(Ok(p)) => {
            if p.subjects.is_empty() {
                ui.label(RichText::new(format!("{} has no commits that aren't in {}/{}.", d.branch, d.base_remote, d.base)).color(WARN));
            } else if let Some(sum) = d.summary() {
                ui.label(RichText::new(sum).color(DIM));
            }
            if let Some(push) = &p.push {
                ui.label(RichText::new(push.describe()).color(DIM));
            }
            if let Some((text, files)) = d.conflict_warning() {
                ui.label(RichText::new(format!("⚠ {text}")).color(WARN));
                for f in files.iter().take(8) {
                    ui.label(RichText::new(format!("    {f}")).small().monospace().color(DIM));
                }
                if files.len() > 8 {
                    ui.label(RichText::new(format!("    and {} more", files.len() - 8)).small().color(DIM));
                }
            }
            if let Some(e) = &p.fetch_error {
                let first = e.lines().find(|l| !l.trim().is_empty()).unwrap_or(e);
                ui.add(egui::Label::new(RichText::new(format!("Couldn't fetch {}/{}, so this compares with the copy fetched before. ({first})", d.base_remote, d.base)).small().color(FAINT)).wrap());
            }
        }
    }
    if let Some(e) = &d.error {
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(crate::app::friendly_error(e)).color(DANGER)).wrap());
    }
    ui.add_space(10.0);

    let can = d.can_create();
    let create_keys = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let label = if d.creating { "Creating…".to_string() } else { format!("Create {noun}") };
        let tip = match &d.ready().and_then(|p| p.push.as_ref()) {
            Some(_) => "Push the branch, then create it (Ctrl/Cmd+Enter)",
            None => "Ctrl/Cmd+Enter",
        };
        if ui.add_enabled(can, egui::Button::new(RichText::new(label).strong()).fill(ACCENT_DIM)).on_hover_text(tip).clicked() {
            out = Outcome::Create;
        }
        if ui.button("Cancel").clicked() {
            out = Outcome::Close;
        }
        if d.creating {
            ui.add(egui::Spinner::new().color(ACCENT));
        }
    });
    if create_keys && can {
        out = Outcome::Create;
    }
    out
}

/// A row picking any number of `options` (reviewers, assignees, labels).
fn picker(ui: &mut Ui, what: &str, options: &[String], chosen: &mut Vec<String>) {
    ui.label(RichText::new(what).color(DIM));
    let text = if chosen.is_empty() { "None".to_string() } else { chosen.join(", ") };
    let combo = egui::ComboBox::from_id_salt(("pr-pick", what))
        .width(420.0)
        .height(260.0)
        .selected_text(text)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            if options.is_empty() {
                ui.label(RichText::new("Nothing to pick").color(FAINT));
            }
            for o in options {
                let mut on = chosen.contains(o);
                if ui.checkbox(&mut on, o).changed() {
                    if on {
                        chosen.push(o.clone());
                    } else {
                        chosen.retain(|c| c != o);
                    }
                }
            }
        });
    combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, what));
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> PrDraft {
        let key = RepoKey { host: "github.com".into(), path: "cnu/headway".into() };
        PrDraft::new("/r".into(), ForgeKind::GitHub, key, "ana".into(), "fix-login-timeout".into(), "upstream".into(), "origin".into(), Some("ana/headway".into()), "main".into())
    }

    fn prep(subjects: &[&str], templates: &[&str]) -> PrPrep {
        PrPrep {
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            files: 7,
            head: "fix-login-timeout".into(),
            templates: templates.iter().enumerate().map(|(i, b)| git::PrTemplate { path: format!("t{i}.md"), body: b.to_string() }).collect(),
            conflicts: Some(vec!["src/a.rs".into(), "src/b.rs".into()]),
            ..Default::default()
        }
    }

    #[test]
    fn filled_in_from_the_commits_until_typed_over() {
        let mut d = draft();
        d.generation = 1;
        d.prepared(1, Ok(prep(&["Add a retry", "Raise the timeout", "Log it"], &[])));
        assert_eq!(d.title, "Fix login timeout");
        assert_eq!(d.body, "- Add a retry\n- Raise the timeout\n- Log it\n");
        assert_eq!(d.summary().as_deref(), Some("3 commits, 7 files changed, into upstream/main"));
        assert_eq!(d.conflict_warning().map(|w| w.0), Some("This will conflict with main in 2 files".into()));

        // Another base: one commit now, and the title follows, unless it was typed over.
        d.generation = 2;
        d.prepared(2, Ok(prep(&["Raise the timeout"], &[])));
        assert_eq!((d.title.as_str(), d.body.as_str()), ("Raise the timeout", "- Raise the timeout\n"));
        d.title = "My own title".into();
        d.generation = 3;
        d.prepared(3, Ok(prep(&["Add a retry", "Raise the timeout"], &[])));
        assert_eq!(d.title, "My own title");
        // An old answer is ignored.
        d.prepared(2, Ok(prep(&["Stale"], &[])));
        assert_eq!(d.body, "- Add a retry\n- Raise the timeout\n");

        let new = d.new_pr().unwrap();
        assert_eq!((new.head.as_str(), new.head_repo.as_deref(), new.base.as_str(), new.title.as_str()), ("fix-login-timeout", Some("ana/headway"), "main", "My own title"));
        assert!(!new.remove_source_branch, "a GitLab option, off on GitHub");
    }

    #[test]
    fn a_template_wins_over_the_commit_list() {
        let mut d = draft();
        d.generation = 1;
        d.prepared(1, Ok(prep(&["One"], &["## What\n", "## Bug\n"])));
        assert_eq!((d.template, d.body.as_str()), (Some(0), "## What\n"));
        assert_eq!(d.summary().as_deref(), Some("1 commit, 7 files changed, into upstream/main"));
        assert!(d.can_create());
        d.title.clear();
        assert!(!d.can_create(), "a title is needed");
        d.title = "x".into();
        d.prepared(1, Ok(prep(&[], &[])));
        assert!(!d.can_create(), "nothing to propose");
    }
}
