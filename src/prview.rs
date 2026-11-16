//! One pull request (merge request) in the repository's window: its
//! description and conversation, commits, changes and checks. Opens over the
//! history and details, like blame and the conflict resolver; Esc closes it.
//!
//! The app fills it section by section from workers (see `app.rs`), so a slow
//! log or a big diff never holds up the description.

use crate::app::{Action, DiffDoc, PrAction, RepoView};
use crate::forge::{self, Check, CheckState, CommentKind, ForgeKind, PrComment, PrState, PullRequest, ReviewDecision};
use crate::git;
use crate::markdown;
use crate::theme::{self, *};
use crate::views::{self, DiffMode, name_it};
use crate::workspace::DiffPrefs;
use egui::{Align, Layout, Margin, RichText, Sense, Stroke, Ui, vec2};
use std::collections::HashMap;
use std::path::Path;

/// Not asked yet or on its way (`None`), or the answer.
pub type Load<T> = Option<Result<T, String>>;

/// How often the checks are read again while any is still running.
pub const AUTO_REFRESH_SECS: f64 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Conversation,
    Commits,
    Files,
    Checks,
}

pub struct PrView {
    pub pr: PullRequest,
    pub kind: ForgeKind,
    /// Which opening this is: answers for an earlier one are dropped.
    pub id: u64,
    /// In front (`false`: put aside for blame or history, with a bar to come back).
    pub shown: bool,
    pub tab: Tab,
    pub body: Load<markdown::Doc>,
    /// The signed-in user may run checks again (once the details are read).
    pub can_rerun: Option<bool>,
    pub checks: Load<Vec<Check>>,
    /// When the checks were last asked for (the app's clock, in seconds).
    pub checks_asked: f64,
    pub conversation: Load<Vec<(PrComment, markdown::Doc)>>,
    /// The head commit, and a note when it's an older copy.
    pub head: Load<(String, Option<String>)>,
    pub commits: Load<Vec<git::Commit>>,
    pub files: Load<DiffDoc>,
    pub files_scroll: Option<usize>,
    /// The commit picked in Commits, and its changes.
    pub commit: Option<String>,
    pub commit_diff: Load<DiffDoc>,
    pub commit_scroll: Option<usize>,
    /// Failed checks' logs shown, by check name (`None` while loading).
    pub logs: HashMap<String, Load<String>>,
    pub rerunning: bool,
}

impl PrView {
    pub fn new(pr: PullRequest, kind: ForgeKind, id: u64) -> Self {
        PrView {
            pr,
            kind,
            id,
            shown: true,
            tab: Tab::Conversation,
            body: None,
            can_rerun: None,
            checks: None,
            checks_asked: 0.0,
            conversation: None,
            head: None,
            commits: None,
            files: None,
            files_scroll: None,
            commit: None,
            commit_diff: None,
            commit_scroll: None,
            logs: HashMap::new(),
            rerunning: false,
        }
    }

    pub fn noun(&self) -> &'static str {
        match self.kind {
            ForgeKind::GitHub => "pull request",
            ForgeKind::GitLab => "merge request",
        }
    }

    /// `#13` on GitHub, `!13` on GitLab.
    pub fn number(&self) -> String {
        match self.kind {
            ForgeKind::GitHub => format!("#{}", self.pr.number),
            ForgeKind::GitLab => format!("!{}", self.pr.number),
        }
    }

    /// Some check is still running (so it's worth asking again soon).
    pub fn pending(&self) -> bool {
        matches!(&self.checks, Some(Ok(c)) if c.iter().any(|c| c.state == CheckState::Pending))
    }

    /// Why "Re-run failed" can't be used, or `None` when it can.
    pub fn rerun_blocked(&self) -> Option<String> {
        if self.rerunning {
            return Some("Starting them again…".into());
        }
        let Some(Ok(checks)) = &self.checks else { return Some("The checks are still loading".into()) };
        match self.can_rerun {
            Some(can) => forge::rerun_blocked(self.kind, can, checks),
            None => forge::rerun_blocked(self.kind, true, checks).or_else(|| Some("Still finding out what you may do here".into())),
        }
    }
}

/// What the view asks the app for.
#[derive(Clone, Debug, PartialEq)]
pub enum PrOp {
    Close,
    /// Bring it back after blame or history.
    Show,
    Refresh,
    SelectCommit(String),
    /// Load a failed check's log (by name), or hide it.
    ShowLog(String),
    HideLog(String),
    Rerun,
}

fn op(o: PrOp) -> Action {
    Action::PrView(o)
}

/// A bar over the history while the view is put aside, to get back to it.
pub fn banner(ui: &mut Ui, rv: &RepoView, actions: &mut Vec<Action>) {
    let Some(pv) = rv.pr_view.as_ref().filter(|p| !p.shown) else { return };
    egui::Panel::top("pr_view_banner")
        .exact_size(32.0)
        .frame(egui::Frame::new().fill(BLUE.gamma_multiply(0.12)).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, BLUE.gamma_multiply(0.4))))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(format!("{} {}", pv.number(), pv.pr.title)).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        actions.push(op(PrOp::Close));
                    }
                    if ui.small_button(format!("Back to {}", pv.number())).clicked() {
                        actions.push(op(PrOp::Show));
                    }
                });
            });
        });
}

pub fn show(ui: &mut Ui, rv: &mut RepoView, prefs: &mut DiffPrefs, actions: &mut Vec<Action>) {
    let keys = !rv.modal && !ui.ctx().text_edit_focused();
    let path = rv.path.clone();
    let Some(pv) = rv.pr_view.as_mut() else { return };
    if keys && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        actions.push(op(PrOp::Close));
    }
    egui::Panel::top("pr_view_header")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin { left: 16, right: 12, top: 10, bottom: 6 }).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            header(ui, pv, &path, actions);
            ui.add_space(6.0);
            tabs(ui, pv);
        });
    match pv.tab {
        Tab::Conversation => conversation(ui, pv),
        Tab::Commits => commits(ui, pv, prefs, &path, actions),
        Tab::Files => files(ui, pv, prefs, &path, actions),
        Tab::Checks => checks(ui, pv, actions),
    }
}

fn state_words(s: PrState) -> &'static str {
    match s {
        PrState::Open => "Open",
        PrState::Draft => "Draft",
        PrState::Merged => "Merged",
        PrState::Closed => "Closed",
    }
}

fn header(ui: &mut Ui, pv: &PrView, path: &Path, actions: &mut Vec<Action>) {
    let pr = &pv.pr;
    let dark = ui.visuals().dark_mode;
    ui.horizontal_top(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            if ui.button("✕").on_hover_text("Close (Esc)").clicked() {
                actions.push(op(PrOp::Close));
            }
            let site = pv.kind.name();
            if ui.button("⟳").on_hover_text("Refresh").clicked() {
                actions.push(op(PrOp::Refresh));
            }
            let open = matches!(pr.state, PrState::Open | PrState::Draft);
            let tip = format!("Fetch {} and switch to a local branch for it", pv.number());
            if ui.add_enabled(open, egui::Button::new("Check out")).on_hover_text(tip).on_disabled_hover_text(format!("This {} is {}", pv.noun(), state_words(pr.state).to_lowercase())).clicked() {
                actions.push(Action::Pr { repo: path.to_path_buf(), number: pr.number, what: PrAction::Checkout });
            }
            if ui.button(format!("Open on {site}")).on_hover_text(&pr.url).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.url));
            }
            // One text, so the number wraps with the title instead of being
            // placed past it, under the buttons.
            let style = ui.style().clone();
            let mut title = egui::text::LayoutJob::default();
            for part in [RichText::new(&pr.title).size(17.0).strong().color(TEXT), RichText::new(format!("  {}", pv.number())).size(17.0).color(DIM)] {
                part.append_to(&mut title, &style, egui::FontSelection::Default, Align::Min);
            }
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                ui.add(egui::Label::new(title).wrap());
            });
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let c = theme::pr_color(pr.state, dark);
        let chip = egui::Button::new(RichText::new(state_words(pr.state)).size(12.0).color(c)).fill(c.gamma_multiply(0.16)).corner_radius(9.0).sense(Sense::hover());
        ui.add(chip);
        ui.label(RichText::new(&pr.author).strong());
        ui.label(RichText::new("wants to merge").color(DIM));
        let head = match &pr.head_repo {
            Some(fork) => format!("{}:{}", fork.split('/').next().unwrap_or(fork), pr.head_branch),
            None => pr.head_branch.clone(),
        };
        let arrow = format!("{} ← {head}", pr.base_branch);
        ui.label(RichText::new(&arrow).monospace().color(TEXT)).on_hover_text(format!("Into {} from {head}", pr.base_branch));
        if pr.state == PrState::Draft {
            ui.label(RichText::new("· draft").color(theme::pr_color(PrState::Draft, dark)));
        }
        let review = match pr.review {
            ReviewDecision::Approved => Some(("· approved", ACCENT)),
            ReviewDecision::ChangesRequested => Some(("· changes requested", WARN)),
            ReviewDecision::ReviewRequired => Some(("· review required", DIM)),
            ReviewDecision::None => None,
        };
        if let Some((w, c)) = review {
            ui.label(RichText::new(w).color(c));
        }
        if !pr.requested_reviewers.is_empty() {
            ui.label(RichText::new(format!("· waiting for {}", pr.requested_reviewers.join(", "))).color(DIM));
        }
        ui.label(RichText::new(format!("· updated {}", theme::relative(pr.updated))).color(FAINT));
    });
}

fn tabs(ui: &mut Ui, pv: &mut PrView) {
    let count = |l: &Load<Vec<git::Commit>>| match l {
        Some(Ok(v)) => format!(" {}", v.len()),
        _ => String::new(),
    };
    let files = match &pv.files {
        Some(Ok(d)) => format!(" {}", d.files.len()),
        _ => String::new(),
    };
    let checks = match &pv.checks {
        Some(Ok(c)) => {
            let failing = c.iter().filter(|c| c.state == CheckState::Failing).count();
            let pending = c.iter().filter(|c| c.state == CheckState::Pending).count();
            if failing > 0 {
                format!(" ✖ {failing}")
            } else if pending > 0 {
                format!(" ● {pending}")
            } else if c.is_empty() {
                String::new()
            } else {
                " ✔".into()
            }
        }
        _ => String::new(),
    };
    let comments = match &pv.conversation {
        Some(Ok(c)) if !c.is_empty() => format!(" {}", c.len()),
        _ => String::new(),
    };
    ui.horizontal(|ui| {
        for (t, label) in [
            (Tab::Conversation, format!("Conversation{comments}")),
            (Tab::Commits, format!("Commits{}", count(&pv.commits))),
            (Tab::Files, format!("Files changed{files}")),
            (Tab::Checks, format!("Checks{checks}")),
        ] {
            let on = pv.tab == t;
            let r = ui.selectable_label(on, RichText::new(&label).color(if on { TEXT } else { DIM }));
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, &label));
            if r.clicked() {
                pv.tab = t;
            }
        }
    });
}

fn loading(ui: &mut Ui, load: &Load<impl Sized>, what: &str) -> bool {
    match load {
        None => {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(20.0).color(ACCENT)));
            true
        }
        Some(Err(e)) => {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.add(egui::Label::new(RichText::new(format!("Couldn't load {what}\n{e}")).color(WARN)).wrap()));
            true
        }
        Some(Ok(_)) => false,
    }
}

// ---------------------------------------------------------------------------
// Conversation

fn conversation(ui: &mut Ui, pv: &PrView) {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(16, 12))).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink(false).id_salt("pr_conversation").show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(900.0));
            card(ui, &format!("{} · description", pv.pr.author), None, |ui| match &pv.body {
                Some(Ok(doc)) if doc.is_empty() => {
                    ui.label(RichText::new("No description").italics().color(DIM));
                }
                Some(Ok(doc)) => markdown::show(ui, doc),
                other => {
                    loading(ui, other, "the description");
                }
            });
            if loading(ui, &pv.conversation, "the conversation") {
                return;
            }
            let Some(Ok(list)) = &pv.conversation else { return };
            for (c, doc) in list {
                let what = match c.kind {
                    CommentKind::Comment => "commented".to_string(),
                    CommentKind::Approved => "approved".to_string(),
                    CommentKind::ChangesRequested => "requested changes".to_string(),
                    CommentKind::Reviewed => "reviewed".to_string(),
                    CommentKind::Line => match (&c.path, c.line) {
                        (Some(p), Some(l)) => format!("commented on {p}:{l}"),
                        (Some(p), None) => format!("commented on {p}"),
                        _ => "commented on the changes".to_string(),
                    },
                };
                let title = format!("{} {what} · {}", c.author, theme::relative(c.time));
                card(ui, &title, c.url.as_deref(), |ui| {
                    if !doc.is_empty() {
                        markdown::show(ui, doc);
                    }
                });
            }
        });
    });
}

fn card(ui: &mut Ui, title: &str, url: Option<&str>, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)).corner_radius(6.0).inner_margin(Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).color(DIM));
            if let Some(u) = url {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.hyperlink_to(RichText::new("↗").color(BLUE), u).on_hover_text(u);
                });
            }
        });
        ui.add_space(4.0);
        body(ui);
    });
    ui.add_space(10.0);
}

// ---------------------------------------------------------------------------
// Commits and changes

fn head_note(ui: &mut Ui, head: &Load<(String, Option<String>)>) {
    if let Some(Ok((_, Some(note)))) = head {
        ui.label(RichText::new(note).small().color(WARN));
    }
}

fn commits(ui: &mut Ui, pv: &mut PrView, prefs: &mut DiffPrefs, path: &Path, actions: &mut Vec<Action>) {
    let total_w = ui.available_width();
    egui::Panel::left("pr_commit_list")
        .resizable(true)
        .default_size((total_w * 0.36).clamp(240.0, 480.0))
        .size_range(200.0..=(total_w * 0.7).max(220.0))
        .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(6, 6)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            head_note(ui, &pv.head);
            if loading(ui, &pv.head, "the commits") {
                return;
            }
            if loading(ui, &pv.commits, "the commits") {
                return;
            }
            let Some(Ok(list)) = &pv.commits else { return };
            if list.is_empty() {
                ui.label(RichText::new("No commits that aren't in the base already").color(DIM));
            }
            egui::ScrollArea::vertical().auto_shrink(false).id_salt("pr_commits").show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for c in list {
                    let selected = pv.commit.as_deref() == Some(c.hash.as_str());
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                    name_it(&resp, egui::Role::Button, &c.summary);
                    let p = ui.painter();
                    if selected {
                        p.rect_filled(rect, 4.0, SELECT);
                    } else if resp.hovered() {
                        p.rect_filled(rect, 4.0, HOVER);
                    }
                    let w = rect.width() - 16.0;
                    let g = views::trunc(p, &c.summary, theme::prop(13.0), TEXT, w);
                    p.galley(rect.left_top() + vec2(8.0, 6.0), g, TEXT);
                    let meta = format!("{} · {} · {}", &c.hash[..8.min(c.hash.len())], c.author, theme::relative(c.time));
                    let g = views::trunc(p, &meta, theme::prop(11.5), DIM, w);
                    p.galley(rect.left_top() + vec2(8.0, 25.0), g, DIM);
                    if resp.clicked() {
                        actions.push(op(PrOp::SelectCommit(c.hash.clone())));
                    }
                }
            });
        });
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
        if pv.commit.is_none() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Pick a commit to see its changes").color(DIM)));
            return;
        }
        if loading(ui, &pv.commit_diff, "the commit's changes") {
            return;
        }
        if let Some(Ok(doc)) = &mut pv.commit_diff {
            changes(ui, doc, &mut pv.commit_scroll, prefs, path, actions);
        }
    });
}

/// A diff with the file counts, fold buttons and diff options above it, as
/// in the commit view.
fn changes(ui: &mut Ui, doc: &mut DiffDoc, scroll: &mut Option<usize>, prefs: &mut DiffPrefs, path: &Path, actions: &mut Vec<Action>) {
    let fold_all = if doc.files.is_empty() { None } else { views::fold_bar(ui, doc, prefs, actions) };
    let click = views::diff_view(ui, doc, DiffMode::Commit, scroll, None, path, actions);
    if let Some(all) = fold_all {
        doc.set_all_collapsed(all);
    }
    if let Some(c) = click {
        doc.apply(c);
    }
}

fn files(ui: &mut Ui, pv: &mut PrView, prefs: &mut DiffPrefs, path: &Path, actions: &mut Vec<Action>) {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
        if loading(ui, &pv.head, "the changes") || loading(ui, &pv.files, "the changes") {
            return;
        }
        if let Some(Ok((_, Some(_)))) = &pv.head {
            egui::Frame::new().inner_margin(Margin::symmetric(12, 4)).show(ui, |ui| head_note(ui, &pv.head));
        }
        if let Some(Ok(doc)) = &mut pv.files {
            changes(ui, doc, &mut pv.files_scroll, prefs, path, actions);
        }
    });
}

// ---------------------------------------------------------------------------
// Checks

/// "45s", "2m 05s", "1h 03m".
pub fn duration_words(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn group_title(s: CheckState) -> &'static str {
    match s {
        CheckState::Failing => "Failing",
        CheckState::Pending => "Running",
        CheckState::Passing => "Passing",
        CheckState::None => "Skipped or neutral",
    }
}

fn mark(s: CheckState) -> &'static str {
    match s {
        CheckState::Failing => "✖",
        CheckState::Pending => "●",
        CheckState::Passing => "✔",
        CheckState::None => "–",
    }
}

fn checks(ui: &mut Ui, pv: &mut PrView, actions: &mut Vec<Action>) {
    let dark = ui.visuals().dark_mode;
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(16, 12))).show(ui, |ui| {
        ui.horizontal(|ui| {
            let blocked = pv.rerun_blocked();
            let label = if pv.rerunning { "Re-running…" } else { "Re-run failed" };
            let tip = match pv.kind {
                ForgeKind::GitHub => "Run the failed jobs of each failed GitHub Actions run again",
                ForgeKind::GitLab => "Retry the failed jobs of the pipeline",
            };
            let b = ui.add_enabled(blocked.is_none(), egui::Button::new(label)).on_hover_text(tip);
            if let Some(why) = &blocked {
                b.clone().on_disabled_hover_text(why);
                if !pv.rerunning {
                    ui.label(RichText::new(why).small().color(DIM));
                }
            }
            if b.clicked() {
                actions.push(op(PrOp::Rerun));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pv.pending() {
                    ui.label(RichText::new(format!("Still running · checking again every {} s", AUTO_REFRESH_SECS as u32)).small().color(DIM));
                }
            });
        });
        ui.add_space(6.0);
        if loading(ui, &pv.checks, "the checks") {
            return;
        }
        let Some(Ok(list)) = &pv.checks else { return };
        if list.is_empty() {
            ui.label(RichText::new(format!("No checks ran on this {}", pv.noun())).color(DIM));
            return;
        }
        let now = theme::now();
        egui::ScrollArea::vertical().auto_shrink(false).id_salt("pr_checks").show(ui, |ui| {
            for (state, group) in forge::group_checks(list) {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{} {}", group_title(state), group.len())).strong().color(theme::check_color(state, dark)));
                for c in group {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(mark(c.state)).color(theme::check_color(c.state, dark)));
                        ui.label(RichText::new(&c.name).color(TEXT));
                        if let Some(d) = c.duration(now) {
                            ui.label(RichText::new(duration_words(d)).small().color(DIM));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some(u) = &c.url {
                                ui.hyperlink_to(RichText::new("Details ↗").small(), u).on_hover_text(u);
                            }
                            if c.state == CheckState::Failing && c.has_log() {
                                let open = pv.logs.contains_key(&c.name);
                                let (text, o) = if open { ("Hide log", PrOp::HideLog(c.name.clone())) } else { ("Show log", PrOp::ShowLog(c.name.clone())) };
                                let r = ui.small_button(text);
                                name_it(&r, egui::Role::Button, format!("{text} of {}", c.name));
                                if r.clicked() {
                                    actions.push(op(o));
                                }
                            }
                        });
                    });
                    if let Some(log) = pv.logs.get(&c.name) {
                        log_box(ui, &c.name, log);
                    }
                }
            }
        });
    });
}

fn log_box(ui: &mut Ui, name: &str, log: &Load<String>) {
    egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)).corner_radius(4.0).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match log {
            None => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                    ui.label(RichText::new("Reading the log…").color(DIM));
                });
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(format!("Couldn't read the log: {e}")).color(WARN)).wrap());
            }
            Some(Ok(text)) => {
                ui.label(RichText::new(format!("The last {} lines of the failed steps", text.lines().count().min(forge::LOG_LINES))).small().color(DIM));
                egui::ScrollArea::both().id_salt(("pr_log", name)).max_height(320.0).stick_to_bottom(true).auto_shrink([false, true]).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(CODE)).extend());
                });
            }
        }
    });
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(duration_words(42), "42s");
        assert_eq!(duration_words(125), "2m 05s");
        assert_eq!(duration_words(3 * 3600 + 7 * 60 + 9), "3h 07m");
    }
}
