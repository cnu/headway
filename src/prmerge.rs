//! The merge dialog for a pull request (merge request): the method the
//! repository allows, the squash commit's title and message, what to delete
//! afterwards, and what's in the way. Merging asks once more (it can't be
//! undone from Headway), then the app does it as one action in the command
//! log, and fetches.

use crate::forge::{self, Blocker, ForgeKind, MergeMethod, MergeReady, MergeRequest, RepoKey};
use crate::theme::*;
use egui::{Align, Layout, RichText, Ui};
use std::path::PathBuf;

pub struct MergeDraft {
    pub repo: PathBuf,
    pub kind: ForgeKind,
    pub key: RepoKey,
    pub number: u64,
    pub pr_title: String,
    pub base: String,
    /// The head commit the view showed (the forge refuses if it moved on).
    pub head: Option<String>,
    pub methods: Vec<MergeMethod>,
    pub method: MergeMethod,
    /// The squash commit's title and message, and what was filled in (to
    /// tell whether they've been typed over).
    pub title: String,
    pub message: String,
    auto_title: String,
    auto_message: String,
    /// The commits' messages are being read (for the squash message).
    pub reading: bool,
    pub delete_branch: bool,
    /// A local branch for it, and whether it's still at the commit merged
    /// (so deleting it loses nothing).
    pub local: Option<(String, bool)>,
    pub delete_local: bool,
    pub blockers: Vec<Blocker>,
    pub ready: MergeReady,
    /// Asking "Merge … into main?" before doing it.
    pub confirming: bool,
    pub merging: bool,
    pub error: Option<String>,
}

/// What the dialog wants done after a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Keep,
    Close,
    Merge,
}

impl MergeDraft {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: PathBuf,
        kind: ForgeKind,
        key: RepoKey,
        pr: &forge::PullRequest,
        facts: &forge::MergeFacts,
        blockers: Vec<Blocker>,
        last: Option<MergeMethod>,
        local: Option<(String, bool)>,
    ) -> Self {
        let method = forge::pick_method(&facts.methods, last).unwrap_or(MergeMethod::Merge);
        let (title, message) = forge::squash_message(kind, &pr.title, pr.number, &[]);
        let ready = forge::merge_ready(kind, &pr.base_branch, facts, &blockers);
        MergeDraft {
            repo,
            kind,
            key,
            number: pr.number,
            pr_title: pr.title.clone(),
            base: pr.base_branch.clone(),
            head: pr.head_oid.clone(),
            methods: facts.methods.clone(),
            method,
            auto_title: title.clone(),
            auto_message: message.clone(),
            title,
            message,
            reading: true,
            delete_branch: facts.delete_branch,
            local,
            delete_local: false,
            blockers,
            ready,
            confirming: false,
            merging: false,
            error: None,
        }
    }

    pub fn number_text(&self) -> String {
        match self.kind {
            ForgeKind::GitHub => format!("#{}", self.number),
            ForgeKind::GitLab => format!("!{}", self.number),
        }
    }

    /// The commits' messages are in: fill in the squash message the way the
    /// forge does, unless it's been typed over.
    pub fn messages_read(&mut self, messages: &[String]) {
        self.reading = false;
        let (title, message) = forge::squash_message(self.kind, &self.pr_title, self.number, messages);
        if self.title == self.auto_title {
            self.title = title.clone();
        }
        if self.message == self.auto_message {
            self.message = message.clone();
        }
        (self.auto_title, self.auto_message) = (title, message);
    }

    /// Turning on GitHub's auto-merge rather than merging now.
    pub fn auto(&self) -> bool {
        self.ready == MergeReady::Auto
    }

    pub fn request(&self) -> MergeRequest {
        MergeRequest {
            method: self.method,
            title: self.title.clone(),
            message: self.message.clone(),
            delete_branch: self.delete_branch,
            auto: self.auto(),
            head: self.head.clone(),
        }
    }

    /// What the last step asks: the method, the base, and that it can't be undone.
    pub fn confirm_text(&self) -> String {
        let site = self.kind.name();
        let what = format!("{} {} into {}", self.method.verb().to_lowercase(), self.number_text(), self.base);
        let then = if self.auto() { format!("{site} will {what} once its checks and reviews are done") } else { format!("{site} will {what} now") };
        format!("{then}. Merging can't be undone from Headway.")
    }
}

/// What happens to the local branch once merged: deleted now (it isn't checked
/// out), or the person is asked to switch to the base and pull first (it is),
/// and deleted after that if they asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AfterMerge {
    pub delete_now: Option<String>,
    pub offer_switch: bool,
    pub delete_after_switch: Option<String>,
}

pub fn after_merge(local: Option<&str>, current: Option<&str>, delete_local: bool) -> AfterMerge {
    let Some(local) = local else { return AfterMerge::default() };
    let delete = delete_local.then(|| local.to_string());
    if current == Some(local) {
        AfterMerge { delete_now: None, offer_switch: true, delete_after_switch: delete }
    } else {
        AfterMerge { delete_now: delete, offer_switch: false, delete_after_switch: None }
    }
}

pub fn ui(ui: &mut Ui, d: &mut MergeDraft) -> Outcome {
    let mut out = Outcome::Keep;
    ui.set_width(560.0);
    ui.heading(format!("Merge {}", d.number_text()));
    ui.label(RichText::new(format!("{} into {}", d.pr_title, d.base)).color(DIM));
    ui.add_space(8.0);
    if d.confirming {
        ui.add(egui::Label::new(RichText::new(d.confirm_text()).color(TEXT)).wrap());
        if let Some(e) = &d.error {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(e).color(WARN)).wrap());
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if d.auto() { "Turn on auto-merge".to_string() } else { d.method.verb().to_string() };
                let label = if d.merging { "Merging…".to_string() } else { label };
                if ui.add_enabled(!d.merging, egui::Button::new(RichText::new(label).strong()).fill(ACCENT_DIM)).clicked() {
                    out = Outcome::Merge;
                }
                if ui.add_enabled(!d.merging, egui::Button::new("Back")).clicked() {
                    d.confirming = false;
                    d.error = None;
                }
                if d.merging {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                }
            });
        });
        return out;
    }

    if !d.blockers.is_empty() {
        egui::Frame::new().fill(WARN.gamma_multiply(0.10)).corner_radius(4.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Not ready to merge").strong().color(WARN));
            for b in &d.blockers {
                ui.add(egui::Label::new(RichText::new(format!("• {}", b.text(d.kind, &d.base))).color(TEXT)).wrap());
            }
            if d.auto() {
                ui.add(egui::Label::new(RichText::new(format!("{} can merge it by itself once these are done.", d.kind.name())).color(DIM)).wrap());
            }
        });
        ui.add_space(8.0);
    }

    ui.label("Method");
    if d.methods.is_empty() {
        ui.label(RichText::new(format!("{} doesn't allow merging here", d.kind.name())).color(WARN));
    }
    for &m in &d.methods.clone() {
        let r = ui.radio_value(&mut d.method, m, m.verb());
        r.on_hover_text(m.about(d.kind));
    }
    ui.add_space(6.0);
    if d.method == MergeMethod::Squash {
        ui.horizontal(|ui| {
            ui.label("Commit title");
            if d.reading {
                ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
            }
        });
        let r = ui.add(egui::TextEdit::singleline(&mut d.title).desired_width(f32::INFINITY));
        crate::views::name_it(&r, egui::Role::TextInput, "Commit title");
        ui.label("Commit message");
        egui::ScrollArea::vertical().id_salt("merge-message").max_height(160.0).show(ui, |ui| {
            let r = ui.add(egui::TextEdit::multiline(&mut d.message).desired_rows(6).desired_width(f32::INFINITY));
            crate::views::name_it(&r, egui::Role::MultilineTextInput, "Commit message");
        });
        ui.add_space(6.0);
    }
    let remote_box = match d.kind {
        ForgeKind::GitHub => "Delete the branch on GitHub after merging",
        ForgeKind::GitLab => "Delete the source branch on GitLab after merging",
    };
    ui.checkbox(&mut d.delete_branch, remote_box);
    if let Some((name, _)) = &d.local {
        let name = name.clone();
        ui.checkbox(&mut d.delete_local, format!("Also delete the local branch “{name}” (can be undone)"));
    }
    if let Some(e) = &d.error {
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(e).color(WARN)).wrap());
    }
    ui.add_space(10.0);
    let blocked = match &d.ready {
        MergeReady::Blocked(why) => Some(why.clone()),
        _ if d.methods.is_empty() => Some(format!("{} doesn't allow merging here", d.kind.name())),
        _ if d.method == MergeMethod::Squash && d.title.trim().is_empty() => Some("Give the commit a title".to_string()),
        _ => None,
    };
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if d.auto() { "Merge when checks pass…".to_string() } else { format!("{}…", d.method.verb()) };
            let b = ui.add_enabled(blocked.is_none(), egui::Button::new(RichText::new(label).strong()).fill(ACCENT_DIM));
            if let Some(why) = &blocked {
                b.clone().on_disabled_hover_text(why);
            }
            if b.clicked() {
                d.confirming = true;
                d.error = None;
            }
            if ui.button("Cancel").clicked() {
                out = Outcome::Close;
            }
            if let Some(why) = &blocked {
                ui.add(egui::Label::new(RichText::new(why).small().color(DIM)).truncate());
            }
        });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merged_branch_is_deleted_now_or_after_switching_away() {
        assert_eq!(after_merge(None, Some("main"), true), AfterMerge::default());
        assert_eq!(after_merge(Some("feature/search"), Some("main"), true), AfterMerge { delete_now: Some("feature/search".into()), ..Default::default() });
        assert_eq!(after_merge(Some("feature/search"), Some("main"), false), AfterMerge::default());
        assert_eq!(
            after_merge(Some("feature/search"), Some("feature/search"), true),
            AfterMerge { delete_now: None, offer_switch: true, delete_after_switch: Some("feature/search".into()) }
        );
        assert_eq!(after_merge(Some("feature/search"), Some("feature/search"), false), AfterMerge { offer_switch: true, ..Default::default() });
    }
}
