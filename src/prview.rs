//! One pull request (merge request) in the repository's window: its
//! description and conversation, commits, changes and checks. Opens over the
//! history and details, like blame and the conflict resolver; Esc closes it.
//!
//! The app fills it section by section from workers (see `app.rs`), so a slow
//! log or a big diff never holds up the description.

use crate::app::{Action, DiffDoc, LineSel, PrAction, RepoView};
use crate::forge::{self, Blocker, Check, CheckState, CommentKind, ForgeKind, LineComment, MergeFacts, PrComment, PrState, PullRequest, ReviewDecision, Side, StateChange, Verdict};
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
    /// What merging and reviewing need (with the details).
    pub facts: Option<MergeFacts>,
    /// The comment being written, and (GitHub) the line comments for the
    /// review. The app keeps it while the view is closed.
    pub draft: ReviewDraft,
    /// The comment's preview, for the text it was made from.
    pub preview: Option<(String, markdown::Doc)>,
    /// Lines picked in Files changed, and the comment being written on them.
    pub files_sel: LineSel,
    pub line_draft: Option<LineDraft>,
    /// A comment, review or state change is on its way.
    pub posting: bool,
}

/// What's been written but not posted yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReviewDraft {
    pub text: String,
    /// Showing the preview instead of the text box.
    pub preview: bool,
    /// GitHub: line comments waiting to go out with the review.
    pub pending: Vec<LineComment>,
}

impl ReviewDraft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.pending.is_empty()
    }
}

/// A comment being written on picked lines (`at` has no body yet).
#[derive(Clone, Debug, PartialEq)]
pub struct LineDraft {
    pub at: LineComment,
    pub text: String,
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
            facts: None,
            draft: ReviewDraft::default(),
            preview: None,
            files_sel: LineSel::default(),
            line_draft: None,
            posting: false,
        }
    }

    /// What keeps it from being merged now (nothing known until the details come).
    pub fn blockers(&self) -> Vec<Blocker> {
        let checks = match &self.checks {
            Some(Ok(c)) => Some(c.as_slice()),
            _ => None,
        };
        self.facts.as_ref().map(|m| forge::blockers(self.kind, &self.pr, m, checks)).unwrap_or_default()
    }

    /// Why the comment box can't post with `verdict` now, or `None` when it can.
    pub fn post_blocked(&self, verdict: Verdict) -> Option<String> {
        if self.posting {
            return Some("Posting…".into());
        }
        let text = self.draft.text.trim().is_empty();
        match verdict {
            Verdict::Comment if text && self.draft.pending.is_empty() => Some("Write something first".into()),
            Verdict::Comment => None,
            _ if self.facts.is_none() => Some("Still reading the details".into()),
            _ if !matches!(self.pr.state, PrState::Open | PrState::Draft) => Some(format!("This {} is {}", self.noun(), state_words(self.pr.state).to_lowercase())),
            Verdict::RequestChanges if text => Some("Say what should change".into()),
            _ => None,
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
    /// Post what's in the comment box (GitHub: with the line comments, as a review).
    Post(Verdict),
    /// The comment written on picked lines: into the review (GitHub) or posted (GitLab).
    AddLineComment,
    SetState(StateChange),
    /// Open the merge dialog.
    Merge,
    /// Turn to this tab (an alert's "Show checks").
    ShowTab(Tab),
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
        // Esc lets go of picked lines first.
        if pv.line_draft.is_some() || !pv.files_sel.lines.is_empty() {
            pv.line_draft = None;
            pv.files_sel = LineSel::default();
        } else {
            actions.push(op(PrOp::Close));
        }
    }
    egui::Panel::top("pr_view_header")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin { left: 16, right: 12, top: 10, bottom: 6 }).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            header(ui, pv, &path, actions);
            ui.add_space(6.0);
            tabs(ui, pv);
        });
    match pv.tab {
        Tab::Conversation => conversation(ui, pv, actions),
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
            state_menu(ui, pv, actions);
            if open {
                merge_button(ui, pv, actions);
            }
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

/// The ⋯ menu: draft and ready, close and reopen (closing asks first).
fn state_menu(ui: &mut Ui, pv: &PrView, actions: &mut Vec<Action>) {
    let noun = pv.noun();
    let changes: &[StateChange] = match pv.pr.state {
        PrState::Draft => &[StateChange::Ready, StateChange::Close],
        PrState::Open => &[StateChange::Draft, StateChange::Close],
        PrState::Closed => &[StateChange::Reopen],
        PrState::Merged => &[],
    };
    if changes.is_empty() {
        return;
    }
    let r = ui.add_enabled_ui(!pv.posting, |ui| {
        ui.menu_button("⋯", |ui| {
            for &c in changes {
                let label = match c {
                    StateChange::Close => format!("Close {noun}…"),
                    StateChange::Reopen => format!("Reopen {noun}"),
                    other => other.label().to_string(),
                };
                if ui.button(label).clicked() {
                    actions.push(match c {
                        StateChange::Close => Action::confirm(
                            &format!("Close {}?", pv.number()),
                            format!("Close {} “{}” without merging it? It can be reopened later.", pv.number(), pv.pr.title),
                            "Close",
                            true,
                            op(PrOp::SetState(c)),
                        ),
                        _ => op(PrOp::SetState(c)),
                    });
                    ui.close();
                }
            }
        })
    });
    r.inner.response.on_hover_text(format!("Draft, ready, close or reopen this {noun}"));
}

/// "Merge" (or "Ready for review" for a draft, which is what's in the way).
fn merge_button(ui: &mut Ui, pv: &PrView, actions: &mut Vec<Action>) {
    if pv.pr.state == PrState::Draft {
        let b = ui.add_enabled(!pv.posting, egui::Button::new("Ready for review")).on_hover_text(format!("Take {} out of draft, so it can be reviewed and merged", pv.number()));
        if b.clicked() {
            actions.push(op(PrOp::SetState(StateChange::Ready)));
        }
        return;
    }
    let tip = match pv.blockers().first() {
        Some(b) => b.text(pv.kind, &pv.pr.base_branch),
        None if pv.facts.is_none() => "Still reading the details".to_string(),
        None => format!("Merge {} into {}", pv.number(), pv.pr.base_branch),
    };
    let b = ui.add_enabled(pv.facts.is_some(), egui::Button::new(RichText::new("Merge…").strong()).fill(ACCENT_DIM)).on_hover_text(tip);
    if b.clicked() {
        actions.push(op(PrOp::Merge));
    }
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

fn conversation(ui: &mut Ui, pv: &mut PrView, actions: &mut Vec<Action>) {
    egui::Panel::bottom("pr_comment_box")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(16, 10)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| comment_box(ui, pv, actions));
    let pv = &*pv;
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

/// Write a comment (with a preview), then post it as a comment or a review.
/// On GitHub, line comments from Files changed wait here for the review.
fn comment_box(ui: &mut Ui, pv: &mut PrView, actions: &mut Vec<Action>) {
    ui.set_max_width(ui.available_width().min(900.0));
    let mut drop = None;
    if !pv.draft.pending.is_empty() {
        let n = pv.draft.pending.len();
        ui.label(RichText::new(format!("Your review: {n} line comment{}", if n == 1 { "" } else { "s" })).strong().color(TEXT));
        for (i, c) in pv.draft.pending.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(c.place()).monospace().small().color(DIM));
                ui.add(egui::Label::new(RichText::new(c.body.lines().next().unwrap_or("")).color(TEXT)).truncate());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let r = ui.small_button("✕").on_hover_text("Remove this line comment");
                    name_it(&r, egui::Role::Button, format!("Remove the comment on {}", c.place()));
                    if r.clicked() {
                        drop = Some(i);
                    }
                });
            });
        }
        ui.add_space(4.0);
    }
    if let Some(i) = drop {
        pv.draft.pending.remove(i);
    }
    ui.horizontal(|ui| {
        for (preview, label) in [(false, "Write"), (true, "Preview")] {
            let on = pv.draft.preview == preview;
            let r = ui.selectable_label(on, RichText::new(label).color(if on { TEXT } else { DIM }));
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, label));
            if r.clicked() {
                pv.draft.preview = preview;
            }
        }
        ui.label(RichText::new("Markdown").small().color(FAINT));
    });
    if pv.draft.preview {
        if pv.preview.as_ref().is_none_or(|(t, _)| *t != pv.draft.text) {
            pv.preview = Some((pv.draft.text.clone(), markdown::parse(&pv.draft.text)));
        }
        egui::Frame::new().fill(BG).stroke(Stroke::new(1.0, BORDER)).corner_radius(4.0).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(70.0);
            egui::ScrollArea::vertical().id_salt("pr_comment_preview").max_height(160.0).show(ui, |ui| match &pv.preview {
                Some((_, doc)) if !doc.is_empty() => markdown::show(ui, doc),
                _ => {
                    ui.label(RichText::new("Nothing to preview").italics().color(DIM));
                }
            });
        });
    } else {
        egui::ScrollArea::vertical().id_salt("pr_comment_text").max_height(160.0).show(ui, |ui| {
            let r = ui.add(egui::TextEdit::multiline(&mut pv.draft.text).hint_text("Leave a comment").desired_rows(4).desired_width(f32::INFINITY));
            name_it(&r, egui::Role::MultilineTextInput, "Comment");
        });
    }
    ui.add_space(6.0);
    let facts = pv.facts.clone().unwrap_or_default();
    let mut verdicts = vec![Verdict::Comment];
    if !facts.viewer_is_author {
        match pv.kind {
            ForgeKind::GitHub => verdicts.extend([Verdict::Approve, Verdict::RequestChanges]),
            ForgeKind::GitLab if facts.viewer_approved => verdicts.push(Verdict::Unapprove),
            ForgeKind::GitLab => verdicts.push(Verdict::Approve),
        }
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for v in verdicts.into_iter().rev() {
            let (label, tip) = match v {
                Verdict::Comment if !pv.draft.pending.is_empty() => ("Submit review", "Post the comment and the line comments as one review"),
                Verdict::Comment => ("Comment", "Post a comment"),
                Verdict::Approve => ("Approve", "Approve the changes (with the comment, if any)"),
                Verdict::RequestChanges => ("Request changes", "Ask for changes before it's merged"),
                Verdict::Unapprove => ("Revoke approval", "Take back your approval (with the comment, if any)"),
            };
            let blocked = pv.post_blocked(v);
            let text = if v == Verdict::Approve { RichText::new(label).strong() } else { RichText::new(label) };
            let mut b = egui::Button::new(text);
            if v == Verdict::Approve {
                b = b.fill(ACCENT_DIM);
            }
            let r = ui.add_enabled(blocked.is_none(), b).on_hover_text(tip);
            if let Some(why) = &blocked {
                r.clone().on_disabled_hover_text(why);
            }
            if r.clicked() {
                actions.push(op(PrOp::Post(v)));
            }
        }
        if pv.posting {
            ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
        }
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
            changes(ui, doc, &mut pv.commit_scroll, None, prefs, path, actions);
        }
    });
}

/// A diff with the file counts, fold buttons and diff options above it, as
/// in the commit view.
fn changes(ui: &mut Ui, doc: &mut DiffDoc, scroll: &mut Option<usize>, sel: Option<&mut LineSel>, prefs: &mut DiffPrefs, path: &Path, actions: &mut Vec<Action>) {
    let fold_all = if doc.files.is_empty() { None } else { views::fold_bar(ui, doc, prefs, actions) };
    let click = views::diff_view(ui, doc, DiffMode::Commit, scroll, sel, path, actions);
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
            line_comment_bar(ui, doc, &mut pv.files_sel, &mut pv.line_draft, pv.kind, pv.posting, actions);
            changes(ui, doc, &mut pv.files_scroll, Some(&mut pv.files_sel), prefs, path, actions);
        }
    });
}

/// What a comment on the picked lines is placed on: the lines of one file,
/// numbered after the change when any of them is still there (added or
/// unchanged), else before it (removed lines).
pub fn line_target(doc: &DiffDoc, sel: &LineSel) -> Option<LineComment> {
    let fi = sel.lines.iter().find(|k| doc.line(**k).is_some())?.0;
    let lines: Vec<&git::DiffLine> = sel.lines.iter().filter(|k| k.0 == fi).filter_map(|k| doc.line(*k)).collect();
    let after: Vec<&&git::DiffLine> = lines.iter().filter(|l| l.new_no.is_some()).collect();
    let (side, picked): (Side, Vec<&&git::DiffLine>) = if after.is_empty() { (Side::Before, lines.iter().collect()) } else { (Side::After, after) };
    let num = |l: &git::DiffLine| if side == Side::After { l.new_no } else { l.old_no };
    let nums: Vec<u32> = picked.iter().filter_map(|l| num(l)).collect();
    let (start, end) = (*nums.iter().min()?, *nums.iter().max()?);
    let last = picked.iter().find(|l| num(l) == Some(end))?;
    let file = &doc.files[fi];
    Some(LineComment {
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        side,
        start,
        end,
        old_line: last.old_no,
        new_line: last.new_no,
        body: String::new(),
    })
}

/// Over the changes while lines are picked: "Comment on these lines", then a
/// box to write it in.
fn line_comment_bar(ui: &mut Ui, doc: &DiffDoc, sel: &mut LineSel, draft: &mut Option<LineDraft>, kind: ForgeKind, posting: bool, actions: &mut Vec<Action>) {
    let Some(at) = line_target(doc, sel) else {
        *draft = None;
        return;
    };
    if draft.as_ref().is_some_and(|d| d.at.place() != at.place() || d.at.side != at.side) {
        // Other lines were picked: the comment goes with them.
        if let Some(d) = draft.as_mut() {
            d.at = at.clone();
        }
    }
    egui::Frame::new().fill(BLUE.gamma_multiply(0.10)).stroke(Stroke::new(1.0, BORDER)).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let n = at.end - at.start + 1;
            let side = if at.side == Side::Before { " (before the change)" } else { "" };
            ui.label(RichText::new(format!("{n} line{} picked", if n == 1 { "" } else { "s" })).strong().color(TEXT));
            ui.label(RichText::new(format!("in {}{side}", at.place())).color(DIM));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Clear").on_hover_text("Esc").clicked() {
                    sel.lines.clear();
                    *draft = None;
                }
                if draft.is_none() && ui.button("Comment on these lines").clicked() {
                    *draft = Some(LineDraft { at: at.clone(), text: String::new() });
                }
            });
        });
        let mut cancel = false;
        let Some(d) = draft.as_mut() else { return };
        let r = ui.add(egui::TextEdit::multiline(&mut d.text).hint_text(format!("Comment on {}", d.at.place())).desired_rows(3).desired_width(f32::INFINITY));
        name_it(&r, egui::Role::MultilineTextInput, "Line comment");
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (label, tip) = match kind {
                    ForgeKind::GitHub => ("Add to review", "Keep it for your review, posted from Conversation with Submit review"),
                    ForgeKind::GitLab => ("Comment", "Post it now, as a discussion on these lines"),
                };
                let empty = d.text.trim().is_empty();
                let b = ui.add_enabled(!empty && !posting, egui::Button::new(RichText::new(label).strong()).fill(ACCENT_DIM)).on_hover_text(tip);
                if empty {
                    b.clone().on_disabled_hover_text("Write something first");
                }
                if b.clicked() {
                    actions.push(op(PrOp::AddLineComment));
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            *draft = None;
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

    /// A comment on picked lines goes on the lines after the change when any
    /// are still there, else on the removed ones; GitLab gets the last line's
    /// numbers on both sides.
    #[test]
    fn picked_lines_say_where_a_comment_goes() {
        let doc = DiffDoc::new(git::parse_patch("diff --git a/old.rs b/new.rs\nsimilarity index 80%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1,4 +1,4 @@\n keep\n-gone\n+added\n same\n-last\n+fresh\n"), true);
        let pick = |lines: &[usize]| LineSel { lines: lines.iter().map(|&li| (0, 0, li)).collect(), ..Default::default() };
        // keep(0) gone(1) added(2) same(3) last(4) fresh(5)
        let c = line_target(&doc, &pick(&[0, 1, 2, 3])).unwrap();
        assert_eq!((c.side, c.start, c.end, c.old_line, c.new_line), (Side::After, 1, 3, Some(3), Some(3)), "an unchanged last line has both numbers");
        assert_eq!((c.path.as_str(), c.old_path.as_deref(), c.place()), ("new.rs", Some("old.rs"), "new.rs:1–3".to_string()));
        let gone = line_target(&doc, &pick(&[1, 4])).unwrap();
        assert_eq!((gone.side, gone.start, gone.end, gone.old_line, gone.new_line), (Side::Before, 2, 4, Some(4), None));
        let one = line_target(&doc, &pick(&[5])).unwrap();
        assert_eq!((one.side, one.place(), one.old_line, one.new_line), (Side::After, "new.rs:4".to_string(), None, Some(4)));
        assert!(line_target(&doc, &LineSel::default()).is_none());
    }

    #[test]
    fn durations_read_like_a_clock() {
        assert_eq!(duration_words(42), "42s");
        assert_eq!(duration_words(125), "2m 05s");
        assert_eq!(duration_words(3 * 3600 + 7 * 60 + 9), "3h 07m");
    }
}
