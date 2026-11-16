//! The interactive rebase editor and the "Fixup into…" picker (dialogs).
//!
//! The plan itself (what each row does, the todo list, the messages) is in
//! `rebase.rs`; these are the windows around it.

use std::path::PathBuf;

use egui::{Align, Layout, Modifiers, RichText, Sense, Stroke, Ui};

use crate::app::{Action, RepoData, dialog_buttons};
use crate::git;
use crate::rebase::{Act, Plan};
use crate::theme::*;

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// The editor: commits oldest to newest, each with what to do with it.
pub struct Editor {
    pub repo: PathBuf,
    pub plan: Plan,
    pub branch: Option<String>,
    /// Uncommitted changes to tracked files: git sets them aside while it works.
    pub dirty: bool,
    /// This git can move the branches in the range along.
    pub update_refs: bool,
    /// What the first commit is rebased onto.
    pub onto: String,
    /// The row the keyboard works on.
    pub selected: usize,
    /// The row being dragged.
    drag: Option<usize>,
    /// Where each row was drawn last frame, to find the gap a drag ends in.
    rows: Vec<egui::Rect>,
}

impl Editor {
    pub fn new(repo: PathBuf, r: git::RebaseRange) -> Editor {
        let last = r.plan.steps.len().saturating_sub(1);
        Editor { repo, plan: r.plan, branch: r.branch, dirty: r.dirty, update_refs: r.update_refs, onto: r.onto, selected: last, drag: None, rows: Vec::new() }
    }

    /// "5 commits → 3".
    pub fn count_text(&self) -> String {
        let (before, after) = self.plan.counts();
        format!("{} → {after}", plural(before, "commit", "commits"))
    }

    /// Move the selected row up or down.
    fn shift(&mut self, delta: isize) {
        let to = self.selected as isize + delta;
        if to >= 0 && (to as usize) < self.plan.steps.len() {
            self.plan.move_to(self.selected, to as usize);
            self.selected = to as usize;
        }
    }

    fn keys(&mut self, ui: &mut Ui) {
        let n = self.plan.steps.len();
        let (up, down) = ui.input_mut(|i| (i.consume_key(Modifiers::ALT, egui::Key::ArrowUp), i.consume_key(Modifiers::ALT, egui::Key::ArrowDown)));
        if up {
            self.shift(-1);
        }
        if down {
            self.shift(1);
        }
        // Plain keys are for the list, not for a message being typed.
        if ui.ctx().egui_wants_keyboard_input() || n == 0 {
            return;
        }
        let (prev, next) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, egui::Key::ArrowUp), i.consume_key(Modifiers::NONE, egui::Key::ArrowDown)));
        if prev && self.selected > 0 {
            self.selected -= 1;
        }
        if next && self.selected + 1 < n {
            self.selected += 1;
        }
        for a in Act::ALL {
            let key = match a {
                Act::Pick => egui::Key::P,
                Act::Reword => egui::Key::R,
                Act::Edit => egui::Key::E,
                Act::Squash => egui::Key::S,
                Act::Fixup => egui::Key::F,
                Act::Drop => egui::Key::D,
            };
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)) && (!a.attaches() || self.plan.can_attach(self.selected)) {
                self.plan.steps[self.selected].act = a;
            }
        }
    }

    /// Show the editor in a dialog. Returns whether to keep it open.
    pub fn ui(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
        let mut keep = true;
        ui.set_width(780.0);
        self.keys(ui);

        let name = self.branch.clone().unwrap_or_else(|| "the current position".into());
        ui.horizontal(|ui| {
            ui.heading(format!("Interactively rebase {name}"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(self.count_text()).strong().color(ACCENT));
            });
        });
        ui.label(RichText::new(format!("Replays these commits, oldest first, on top of {}. Drag a row or press Alt+↑ / Alt+↓ to move it; P R E S F D set what happens to it.", self.onto)).small().color(DIM));
        ui.add_space(6.0);

        let max = (ui.ctx().content_rect().height() * 0.5).max(160.0);
        egui::ScrollArea::vertical().max_height(max).auto_shrink([false, true]).show(ui, |ui| self.rows_ui(ui));

        ui.add_space(6.0);
        let auto = self.plan.autosquash_count();
        if auto > 0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} fixup!/squash! can be placed under the commit they're for.", plural(auto, "commit", "commits"))).color(WARN));
                if ui.button("Place them automatically").clicked() {
                    self.plan.autosquash();
                }
            });
        }
        if let Some(p) = self.plan.problem() {
            ui.label(RichText::new(format!("⚠ {p}")).color(DANGER));
        }
        if self.plan.steps.iter().any(|s| s.pushed) {
            ui.label(RichText::new("⚠ Some of these commits are already on a remote. After rebasing you'll need to force-push, and anyone who pulled them will have to catch up.").small().color(WARN));
        }
        let moving: Vec<String> = self.plan.steps.iter().flat_map(|s| s.branches.iter().map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())).collect();
        if !moving.is_empty() && self.update_refs {
            ui.label(RichText::new(format!("Branches that move along with their commits: {}.", moving.join(", "))).small().color(DIM));
        } else if !moving.is_empty() {
            ui.label(RichText::new(format!("This git is older than 2.38, so {} won't move along with their commits.", moving.join(", "))).small().color(WARN));
        }
        if self.dirty {
            ui.label(RichText::new("Your uncommitted changes are set aside while this runs and put back afterwards.").small().color(DIM));
        }
        ui.label(RichText::new("Tags stay on the old commits. If it stops, you can continue, skip the commit or abort from the bar at the top; Undo takes the whole rebase back.").small().color(DIM));
        ui.add_space(8.0);

        let valid = self.plan.problem().is_none() && !self.plan.is_unchanged();
        let (ok, cancel) = dialog_buttons(ui, "Start rebase", valid, false);
        if ok && valid {
            let plan = self.plan.clone();
            let autostash = self.dirty;
            actions.push(
                Action::git(&self.repo, "Interactive rebase", move |r| git::rebase_interactive(r, &plan, autostash))
                    .undoable(git::Undoable::Rebase { save_work: false }, "interactive rebase"),
            );
            keep = false;
        }
        if cancel {
            keep = false;
        }
        keep
    }

    fn rows_ui(&mut self, ui: &mut Ui) {
        let chains = self.plan.chains();
        let n = self.plan.steps.len();
        let mut rects = Vec::with_capacity(n);
        let mut dropped_at: Option<(usize, usize)> = None;
        for i in 0..n {
            let (hash, short, summary, act, pushed, branches) = {
                let s = &self.plan.steps[i];
                (s.hash.clone(), s.short().to_string(), s.summary.clone(), s.act, s.pushed, s.branches.clone())
            };
            let can_attach = self.plan.can_attach(i);
            let attached = act.attaches();
            let frame = egui::Frame::new()
                .fill(if self.selected == i { SELECT } else { egui::Color32::TRANSPARENT })
                .inner_margin(egui::Margin::symmetric(6, 3))
                .corner_radius(4.0);
            let shown = frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    if attached {
                        ui.add_space(30.0);
                        ui.label(RichText::new("└").color(FAINT));
                    }
                    let handle = ui.add(egui::Label::new(RichText::new("⠿").color(DIM)).sense(Sense::drag())).on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to move");
                    if handle.drag_started() {
                        self.drag = Some(i);
                        self.selected = i;
                    }
                    if handle.drag_stopped() {
                        let y = ui.input(|inp| inp.pointer.latest_pos()).map_or(0.0, |p| p.y);
                        let gap = self.rows.iter().filter(|r| r.center().y < y).count();
                        dropped_at = Some((i, if gap > i { gap - 1 } else { gap }));
                    }
                    let mut chosen = act;
                    egui::ComboBox::from_id_salt(("rebase-act", &hash)).selected_text(act.word()).width(78.0).show_ui(ui, |ui| {
                        for a in Act::ALL {
                            let usable = !a.attaches() || can_attach;
                            ui.add_enabled_ui(usable, |ui| ui.selectable_value(&mut chosen, a, a.word()).on_hover_text(a.help()));
                        }
                    });
                    if chosen != act {
                        self.plan.steps[i].act = chosen;
                        self.selected = i;
                    }
                    ui.label(RichText::new(&short).monospace().color(DIM));
                    let mut text = RichText::new(&summary);
                    if act == Act::Drop {
                        text = text.strikethrough().color(FAINT);
                    } else if attached {
                        text = text.color(DIM);
                    }
                    if ui.add(egui::Label::new(text).truncate().sense(Sense::click())).clicked() {
                        self.selected = i;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pushed {
                            ui.label(RichText::new("on a remote").small().color(WARN));
                        }
                        for b in branches.iter().rev() {
                            ui.label(RichText::new(b.strip_prefix("refs/heads/").unwrap_or(b)).small().color(BLUE));
                        }
                    });
                });
            });
            rects.push(shown.response.rect);

            // A reword's message, right under it (unless the chain it heads has one message for all).
            let heads_squash = chains.iter().any(|c| c.has_squash && c.head() == i);
            if self.plan.steps[i].act == Act::Reword && !heads_squash {
                ui.horizontal(|ui| {
                    ui.add_space(36.0);
                    let te = egui::TextEdit::multiline(&mut self.plan.steps[i].message).id_salt(("rebase-msg", &hash)).desired_rows(3).desired_width(f32::INFINITY);
                    ui.add(te);
                });
            }
            // The one message a melted chain gets.
            if let Some(c) = chains.iter().find(|c| c.has_squash && c.last() == i) {
                let mut text = self.plan.combined(c);
                let before = text.clone();
                ui.horizontal(|ui| {
                    ui.add_space(36.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Message for the combined commit").small().color(DIM));
                        ui.add(egui::TextEdit::multiline(&mut text).id_salt(("rebase-combined", &hash)).desired_rows(4).desired_width(f32::INFINITY));
                    });
                });
                if text != before {
                    self.plan.set_combined(c, text);
                }
            }
        }
        // Where a drag would drop, drawn as a line between rows.
        if let (Some(_), Some(p)) = (self.drag, ui.input(|i| i.pointer.latest_pos())) {
            if ui.input(|i| i.pointer.any_down()) {
                let gap = self.rows.iter().filter(|r| r.center().y < p.y).count();
                if let Some(r) = self.rows.get(gap).map(|r| r.top()).or_else(|| self.rows.last().map(|r| r.bottom())) {
                    let x = self.rows.first().map_or(ui.min_rect().left()..=ui.min_rect().right(), |r| r.left()..=r.right());
                    ui.painter().line_segment([egui::pos2(*x.start(), r), egui::pos2(*x.end(), r)], Stroke::new(2.0, ACCENT));
                }
            } else {
                self.drag = None;
            }
        }
        if let Some((from, to)) = dropped_at {
            self.plan.move_to(from, to);
            self.selected = to;
            self.drag = None;
        }
        self.rows = rects;
    }
}

// ---------------------------------------------------------------------------
// Fixup into…

/// Pick the commit the staged changes belong in.
pub struct FixupPicker {
    pub repo: PathBuf,
    /// (commit, summary, on a remote), newest first.
    pub commits: Vec<(String, String, bool)>,
    pub filter: String,
    pub picked: Option<String>,
    /// Older commits aren't offered: this merge commit is in the way.
    pub merge: Option<String>,
}

/// How many commits to offer.
const OFFERED: usize = 200;

impl FixupPicker {
    /// The loaded commits from HEAD back to the first merge commit.
    pub fn new(repo: PathBuf, data: &RepoData) -> FixupPicker {
        let by_hash: std::collections::HashMap<&str, &git::Commit> = data.commits.iter().map(|c| (c.hash.as_str(), c)).collect();
        let mut commits = Vec::new();
        let mut merge = None;
        let mut at = data.status.head.oid.as_deref().and_then(|h| by_hash.get(h).copied());
        while let Some(c) = at {
            if c.parents.len() > 1 {
                merge = Some(format!("{} “{}”", c.short(), c.summary));
                break;
            }
            if commits.len() >= OFFERED {
                break;
            }
            commits.push((c.hash.clone(), c.summary.clone(), data.pushed.contains(&c.hash)));
            at = c.parents.first().and_then(|p| by_hash.get(p.as_str()).copied());
        }
        let picked = commits.first().map(|c| c.0.clone());
        FixupPicker { repo, commits, filter: String::new(), picked, merge }
    }

    pub fn ui(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
        let mut keep = true;
        ui.set_width(560.0);
        ui.heading("Fix up into…");
        ui.label(RichText::new("Your staged changes become part of the commit you pick. Headway makes a fixup commit and folds it in, moving every commit after it (git also folds in any other fixup!/squash! commit already after it).").small().color(DIM));
        ui.add_space(6.0);
        let filter = ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter commits").desired_width(f32::INFINITY));
        if self.filter.is_empty() && !filter.has_focus() {
            filter.request_focus();
        }
        ui.add_space(4.0);
        let needle = self.filter.trim().to_lowercase();
        let visible: Vec<usize> = (0..self.commits.len())
            .filter(|&i| {
                let (hash, summary, _) = &self.commits[i];
                needle.is_empty() || summary.to_lowercase().contains(&needle) || hash.starts_with(&needle)
            })
            .collect();
        // What's picked must be on the list: filtering moves the pick to the first match.
        if !visible.iter().any(|&i| Some(&self.commits[i].0) == self.picked.as_ref()) {
            self.picked = visible.first().map(|&i| self.commits[i].0.clone());
        }
        egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
            for &i in &visible {
                let (hash, summary, _) = &self.commits[i];
                let on = self.picked.as_deref() == Some(hash.as_str());
                if ui.selectable_label(on, format!("{}  {summary}", &hash[..hash.len().min(8)])).clicked() {
                    self.picked = Some(hash.clone());
                }
            }
            if visible.is_empty() {
                ui.label(RichText::new("No commit matches.").color(DIM));
            }
        });
        if let Some(m) = &self.merge {
            ui.label(RichText::new(format!("Older commits aren't offered: the merge commit {m} is in the way, and a rebase from before it would flatten it.")).small().color(DIM));
        }
        let pushed = self.picked.as_ref().and_then(|p| self.commits.iter().find(|c| &c.0 == p)).is_some_and(|c| c.2);
        if pushed {
            ui.label(RichText::new("⚠ That commit is already on a remote. You'll need to force-push afterwards.").small().color(WARN));
        }
        ui.add_space(8.0);
        let (ok, cancel) = dialog_buttons(ui, "Fix up", self.picked.is_some(), false);
        if ok && let Some(h) = self.picked.clone() {
            let short = h[..h.len().min(7)].to_string();
            actions.push(
                Action::git(&self.repo, format!("Fix up into {short}"), move |r| git::fixup_into(r, &h))
                    .undoable(git::Undoable::Rebase { save_work: true }, format!("fix up into {short}")),
            );
            keep = false;
        }
        if cancel {
            keep = false;
        }
        keep
    }
}
