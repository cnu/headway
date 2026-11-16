//! The review inbox on the workspace overview: pull requests across every
//! repository of the workspace that need the signed-in user, grouped by what
//! to do about them; and the alerts for what changed since the last look.
//!
//! The app reads each host's inbox through the forge cache (one call per
//! host); everything here is worked out from those answers, so it's tested
//! without a forge.

use crate::app::Action;
use crate::forge::{self, CheckState, ForgeKind, Inbox, InboxPr, PrState, RepoKey, ReviewDecision};
use crate::theme::{self, *};
use crate::views::{self, name_it};
use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One pull request, across repositories (as kept in workspaces.json).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: RepoKey,
    pub number: u64,
}

impl PrRef {
    pub fn of(i: &InboxPr) -> Self {
        PrRef { repo: i.repo.clone(), number: i.pr.number }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snooze {
    pub pr: PrRef,
    /// Unix seconds.
    pub until: i64,
}

/// What can alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    ChecksFailed,
    Approved,
    ChangesRequested,
    ReviewRequested,
    Merged,
}

impl AlertKind {
    pub const ALL: [AlertKind; 5] = [AlertKind::ChecksFailed, AlertKind::Approved, AlertKind::ChangesRequested, AlertKind::ReviewRequested, AlertKind::Merged];

    /// For the Alerts menu.
    pub fn label(self) -> &'static str {
        match self {
            AlertKind::ChecksFailed => "Checks failed on my pull requests",
            AlertKind::Approved => "My pull requests approved",
            AlertKind::ChangesRequested => "Changes requested on mine",
            AlertKind::ReviewRequested => "A review requested from me",
            AlertKind::Merged => "My pull requests merged",
        }
    }
}

/// A workspace's inbox settings, saved with it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InboxPrefs {
    /// Shown at the top, in this order.
    pub pinned: Vec<PrRef>,
    pub snoozed: Vec<Snooze>,
    /// Alerts turned off.
    pub alerts_off: Vec<AlertKind>,
}

impl InboxPrefs {
    pub fn pin(&mut self, pr: PrRef) {
        self.snoozed.retain(|s| s.pr != pr);
        if !self.pinned.contains(&pr) {
            self.pinned.push(pr);
        }
    }

    pub fn unpin(&mut self, pr: &PrRef) {
        self.pinned.retain(|p| p != pr);
    }

    /// Out of the way until `until` (a snoozed one isn't pinned).
    pub fn snooze(&mut self, pr: PrRef, until: i64) {
        self.pinned.retain(|p| *p != pr);
        self.snoozed.retain(|s| s.pr != pr);
        self.snoozed.push(Snooze { pr, until });
    }

    pub fn wake(&mut self, pr: &PrRef) {
        self.snoozed.retain(|s| s.pr != *pr);
    }

    pub fn snoozed_until(&self, pr: &PrRef, now: i64) -> Option<i64> {
        self.snoozed.iter().find(|s| s.pr == *pr && s.until > now).map(|s| s.until)
    }

    /// Forget snoozes that are over, and pins and snoozes of pull requests
    /// that are no longer in the inbox (merged, closed, review given).
    pub fn tidy(&mut self, present: &[PrRef], now: i64) {
        self.snoozed.retain(|s| s.until > now && present.contains(&s.pr));
        self.pinned.retain(|p| present.contains(p));
    }

    pub fn alert_on(&self, kind: AlertKind) -> bool {
        !self.alerts_off.contains(&kind)
    }
}

/// The inbox's groups, in the order shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Pinned,
    NeedsReview,
    Failing,
    ChangesRequested,
    Ready,
    Mine,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Pinned => "Pinned",
            Group::NeedsReview => "Needs your review",
            Group::Failing => "Your pull requests with failing checks",
            Group::ChangesRequested => "Your pull requests with changes requested",
            Group::Ready => "Your pull requests ready to merge",
            Group::Mine => "Your other open pull requests",
        }
    }

    /// Something to do (counted on the tab).
    pub fn needs_action(self) -> bool {
        !matches!(self, Group::Pinned | Group::Mine)
    }
}

/// Where a pull request goes (pins aside): asking for my review, else mine
/// by what's next for it — fix the checks, answer the review, merge it — or
/// just open.
pub fn classify(pr: &forge::PullRequest, review: bool) -> Group {
    if review {
        return Group::NeedsReview;
    }
    if pr.checks.state == CheckState::Failing {
        return Group::Failing;
    }
    match pr.review {
        ReviewDecision::ChangesRequested => Group::ChangesRequested,
        ReviewDecision::Approved if pr.state == PrState::Open && matches!(pr.checks.state, CheckState::Passing | CheckState::None) && pr.mergeable != forge::Mergeable::Conflicts => Group::Ready,
        _ => Group::Mine,
    }
}

/// A pull request as the inbox lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub pr: InboxPr,
    /// Asking for my review (rather than mine).
    pub review: bool,
    /// What it would be without its pin.
    pub group: Group,
    pub pinned: bool,
    pub snoozed_until: Option<i64>,
}

/// Everything in the inbox, grouped: pinned ones first (in pin order), then
/// each group, newest first; snoozed ones apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grouped {
    pub groups: Vec<(Group, Vec<Item>)>,
    pub snoozed: Vec<Item>,
}

impl Grouped {
    /// How many need something done (snoozed ones don't).
    pub fn action_count(&self) -> usize {
        self.groups.iter().flat_map(|g| &g.1).filter(|i| i.group.needs_action()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.snoozed.is_empty()
    }
}

pub fn group(inboxes: &[&Inbox], prefs: &InboxPrefs, now: i64) -> Grouped {
    let mut items: Vec<Item> = Vec::new();
    for inbox in inboxes {
        for (list, review) in [(&inbox.review, true), (&inbox.mine, false)] {
            for pr in list {
                if items.iter().any(|i| i.pr.id() == pr.id()) {
                    continue;
                }
                let r = PrRef::of(pr);
                items.push(Item { group: classify(&pr.pr, review), review, pinned: prefs.pinned.contains(&r), snoozed_until: prefs.snoozed_until(&r, now), pr: pr.clone() });
            }
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.pr.pr.updated));
    let (snoozed, items): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|i| i.snoozed_until.is_some());
    let mut out = Grouped { snoozed, ..Default::default() };
    let mut pinned: Vec<Item> = items.iter().filter(|i| i.pinned).cloned().collect();
    pinned.sort_by_key(|i| prefs.pinned.iter().position(|p| *p == PrRef::of(&i.pr)));
    if !pinned.is_empty() {
        out.groups.push((Group::Pinned, pinned));
    }
    for g in [Group::NeedsReview, Group::Failing, Group::ChangesRequested, Group::Ready, Group::Mine] {
        let list: Vec<Item> = items.iter().filter(|i| !i.pinned && i.group == g).cloned().collect();
        if !list.is_empty() {
            out.groups.push((g, list));
        }
    }
    out
}

/// Something that changed for the signed-in user.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub kind: AlertKind,
    pub pr: InboxPr,
}

impl Alert {
    /// Alerts the same thing only once: the event and the commit it's about.
    pub fn key(&self) -> String {
        format!("{:?} {} {} {}", self.kind, self.pr.repo, self.pr.pr.number, self.pr.pr.head_oid.as_deref().unwrap_or(""))
    }

    /// What the notification says.
    pub fn text(&self, kind: ForgeKind) -> String {
        let n = match kind {
            ForgeKind::GitHub => format!("#{}", self.pr.pr.number),
            ForgeKind::GitLab => format!("!{}", self.pr.pr.number),
        };
        let what = format!("{n} {}", self.pr.pr.title);
        match self.kind {
            AlertKind::ChecksFailed => format!("Checks failed on {what}"),
            AlertKind::Approved => format!("{what} was approved"),
            AlertKind::ChangesRequested => format!("Changes were requested on {what}"),
            AlertKind::ReviewRequested => format!("{} asks for your review on {what}", self.pr.pr.author),
            AlertKind::Merged => format!("{what} was merged into {}", self.pr.pr.base_branch),
        }
    }
}

/// What changed between two answers from the same host. Only pull requests of
/// repositories asked about both times count (a repository just added to the
/// workspace isn't news), and only changes: what was already so the first
/// time says nothing.
pub fn alerts(before: &Inbox, after: &Inbox) -> Vec<Alert> {
    let known = |i: &InboxPr| before.repos.contains(&i.repo);
    let find = |list: &[InboxPr], i: &InboxPr| list.iter().find(|b| b.id() == i.id()).cloned();
    let mut out = Vec::new();
    for now in after.mine.iter().filter(|i| known(i)) {
        let Some(was) = find(&before.mine, now) else { continue };
        if now.pr.checks.state == CheckState::Failing && was.pr.checks.state != CheckState::Failing {
            out.push(Alert { kind: AlertKind::ChecksFailed, pr: now.clone() });
        }
        if now.pr.review != was.pr.review {
            match now.pr.review {
                ReviewDecision::Approved => out.push(Alert { kind: AlertKind::Approved, pr: now.clone() }),
                ReviewDecision::ChangesRequested => out.push(Alert { kind: AlertKind::ChangesRequested, pr: now.clone() }),
                _ => {}
            }
        }
    }
    for now in after.review.iter().filter(|i| known(i)) {
        if find(&before.review, now).is_none() {
            out.push(Alert { kind: AlertKind::ReviewRequested, pr: now.clone() });
        }
    }
    for now in after.merged.iter().filter(|i| known(i)) {
        if find(&before.merged, now).is_none() {
            out.push(Alert { kind: AlertKind::Merged, pr: now.clone() });
        }
    }
    out
}

/// Until when "Until tomorrow" and "Until next week" snooze: a day or a week
/// from now.
pub fn tomorrow(now: i64) -> i64 {
    now + 24 * 3600
}

pub fn next_week(now: i64) -> i64 {
    now + 7 * 24 * 3600
}

// ---------------------------------------------------------------------------
// Drawing

/// What the Inbox tab shows (the app puts it together).
#[derive(Default)]
pub struct InboxView {
    pub grouped: Grouped,
    /// Each row's repository folder and name, by its key.
    pub repos: Vec<(RepoKey, PathBuf, String, ForgeKind)>,
    /// Repositories on a forge that can't be reached: name and how to fix it.
    pub not_connected: Vec<(String, forge::StatusLine)>,
    /// Some host hasn't answered yet.
    pub loading: bool,
    /// Hosts whose last answer failed, with why.
    pub errors: Vec<String>,
    pub alerts_off: Vec<AlertKind>,
}

/// What the tab asks the app for.
#[derive(Clone, Debug, PartialEq)]
pub enum InboxOp {
    Pin(PrRef),
    Unpin(PrRef),
    Snooze(PrRef, i64),
    Wake(PrRef),
    Alert(AlertKind, bool),
    Refresh,
}

fn op(o: InboxOp) -> Action {
    Action::Inbox(o)
}

pub fn show(ui: &mut Ui, v: &InboxView, actions: &mut Vec<Action>) {
    ui.set_max_width(ui.available_width().min(1100.0));
    ui.horizontal(|ui| {
        let n = v.grouped.action_count();
        let summary = match n {
            0 if v.grouped.is_empty() => "Nothing waiting for you".to_string(),
            0 => "Nothing needs you right now".to_string(),
            n => format!("{n} need{} you", if n == 1 { "s" } else { "" }),
        };
        ui.label(RichText::new(summary).color(DIM));
        if v.loading {
            ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.menu_button("Alerts", |ui| {
                ui.label(RichText::new("Tell me when").small().color(DIM));
                for k in AlertKind::ALL {
                    let mut on = !v.alerts_off.contains(&k);
                    if ui.checkbox(&mut on, k.label()).changed() {
                        actions.push(op(InboxOp::Alert(k, on)));
                    }
                }
            })
            .response
            .on_hover_text("Which changes notify you");
            if ui.button("⟳").on_hover_text("Ask GitHub and GitLab again").clicked() {
                actions.push(op(InboxOp::Refresh));
            }
        });
    });
    for e in &v.errors {
        ui.add(egui::Label::new(RichText::new(e).small().color(WARN)).wrap());
    }
    ui.add_space(8.0);
    egui::ScrollArea::vertical().auto_shrink(false).id_salt("inbox").show(ui, |ui| {
        for (g, items) in &v.grouped.groups {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{} {}", g.title(), items.len())).strong().color(if g.needs_action() { TEXT } else { DIM }));
            for i in items {
                row(ui, v, i, actions);
            }
        }
        if !v.grouped.snoozed.is_empty() {
            ui.add_space(10.0);
            let id = egui::Id::unique("inbox_show_snoozed");
            let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
            let n = v.grouped.snoozed.len();
            let label = format!("{n} snoozed · {}", if open { "Hide" } else { "Show" });
            if ui.add(egui::Button::new(RichText::new(&label).color(DIM)).frame(false)).clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(id, open));
            }
            if open {
                for i in &v.grouped.snoozed {
                    row(ui, v, i, actions);
                }
            }
        }
        if v.grouped.is_empty() && !v.loading && v.not_connected.len() < v.repos.len().max(1) {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("No open pull requests need you in this workspace").color(DIM)));
        }
        if !v.not_connected.is_empty() {
            ui.add_space(16.0);
            ui.label(RichText::new(format!("Not connected {}", v.not_connected.len())).strong().color(DIM));
            for (name, line) in &v.not_connected {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).color(TEXT));
                    ui.label(RichText::new(line.text()).small().color(DIM));
                    if let Some(url) = line.link() {
                        ui.hyperlink_to(RichText::new(url.trim_start_matches("https://")).small(), url);
                    }
                    if let Some(cmd) = line.command()
                        && ui.small_button("Copy command").on_hover_text(&cmd).clicked()
                    {
                        actions.push(Action::Copy(cmd));
                    }
                });
            }
        }
    });
}

fn row(ui: &mut Ui, v: &InboxView, i: &Item, actions: &mut Vec<Action>) {
    let Some((_, path, name, kind)) = v.repos.iter().find(|r| r.0 == i.pr.repo) else { return };
    let pr = &i.pr.pr;
    let dark = ui.visuals().dark_mode;
    let number = match kind {
        ForgeKind::GitHub => format!("#{}", pr.number),
        ForgeKind::GitLab => format!("!{}", pr.number),
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    name_it(&resp, egui::Role::Button, format!("{name} {number} {}", pr.title));
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, HOVER);
    }
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER.gamma_multiply(0.5)));
    // The check dot.
    let dot = rect.left_top() + vec2(12.0, 16.0);
    let c = theme::check_color(pr.checks.state, dark);
    if pr.checks.state == CheckState::None {
        p.circle_stroke(dot, 4.0, Stroke::new(1.0, FAINT));
    } else {
        p.circle_filled(dot, 4.5, c);
    }
    let x = rect.left() + 26.0;
    let w = rect.width() - 26.0 - 200.0;
    let head = format!("{name} {number}");
    let g = views::trunc(p, &head, theme::prop(12.5), DIM, 160.0);
    let head_w = g.size().x;
    p.galley(egui::pos2(x, rect.top() + 7.0), g, DIM);
    let title = views::trunc(p, &pr.title, theme::prop(13.5), TEXT, (w - head_w - 10.0).max(40.0));
    p.galley(egui::pos2(x + head_w + 8.0, rect.top() + 6.0), title, TEXT);
    let review = match pr.review {
        ReviewDecision::Approved => " · approved",
        ReviewDecision::ChangesRequested => " · changes requested",
        ReviewDecision::ReviewRequired => " · review required",
        ReviewDecision::None => "",
    };
    let checks = match pr.checks.state {
        CheckState::Failing => " · checks failing",
        CheckState::Pending => " · checks running",
        CheckState::Passing => " · checks passing",
        CheckState::None => "",
    };
    let draft = if pr.state == PrState::Draft { " · draft" } else { "" };
    let mut meta = format!("{}{draft}{checks}{review} · {}", pr.author, theme::relative(pr.updated));
    if let Some(t) = i.snoozed_until {
        meta.push_str(&format!(" · snoozed until {}", theme::relative(t).replace(" ago", "")));
    }
    let g = views::trunc(p, &meta, theme::prop(11.5), DIM, w);
    p.galley(egui::pos2(x, rect.top() + 26.0), g, DIM);
    if i.pinned {
        p.text(rect.right_top() + vec2(-44.0, 14.0), egui::Align2::RIGHT_CENTER, "📌", theme::prop(12.0), DIM);
    }
    if resp.clicked() {
        actions.push(Action::OpenPr { repo: path.clone(), pr: Box::new(pr.clone()) });
    }
    // The ⋯ menu, over the row's right end.
    let r = PrRef::of(&i.pr);
    let menu_rect = egui::Rect::from_min_size(rect.right_top() + vec2(-34.0, 8.0), vec2(28.0, 26.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(menu_rect).layout(Layout::left_to_right(Align::Center)));
    let m = child.menu_button("⋯", |ui| {
        if i.pinned {
            if ui.button("Unpin").clicked() {
                actions.push(op(InboxOp::Unpin(r.clone())));
                ui.close();
            }
        } else if ui.button("Pin to the top").clicked() {
            actions.push(op(InboxOp::Pin(r.clone())));
            ui.close();
        }
        if i.snoozed_until.is_some() {
            if ui.button("Stop snoozing").clicked() {
                actions.push(op(InboxOp::Wake(r.clone())));
                ui.close();
            }
        } else {
            let now = theme::now();
            if ui.button("Snooze until tomorrow").clicked() {
                actions.push(op(InboxOp::Snooze(r.clone(), tomorrow(now))));
                ui.close();
            }
            if ui.button("Snooze until next week").clicked() {
                actions.push(op(InboxOp::Snooze(r.clone(), next_week(now))));
                ui.close();
            }
        }
        if ui.button("Open on the web").clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&pr.url));
            ui.close();
        }
    });
    name_it(&m.response, egui::Role::Button, format!("More for {name} {number}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::{CheckSummary, Mergeable, PullRequest};

    fn key(path: &str) -> RepoKey {
        RepoKey { host: "github.com".into(), path: path.into() }
    }

    fn pr(n: u64, author: &str, checks: CheckState, review: ReviewDecision, updated: i64) -> InboxPr {
        let states = match checks {
            CheckState::None => vec![],
            s => vec![(s, 1)],
        };
        InboxPr {
            repo: key("acme/shop"),
            pr: PullRequest {
                number: n,
                title: format!("PR {n}"),
                state: PrState::Open,
                author: author.into(),
                head_branch: format!("b{n}"),
                base_branch: "main".into(),
                head_repo: None,
                url: String::new(),
                review,
                requested_reviewers: vec![],
                checks: CheckSummary::from_states(states),
                mergeable: Mergeable::Clean,
                updated,
                head_oid: Some(format!("oid{n}")),
            },
        }
    }

    fn sample() -> Inbox {
        Inbox {
            repos: vec![key("acme/shop")],
            review: vec![pr(1, "sam", CheckState::Passing, ReviewDecision::ReviewRequired, 50)],
            mine: vec![
                pr(2, "me", CheckState::Failing, ReviewDecision::ChangesRequested, 40),
                pr(3, "me", CheckState::Passing, ReviewDecision::ChangesRequested, 30),
                pr(4, "me", CheckState::Passing, ReviewDecision::Approved, 20),
                pr(5, "me", CheckState::Pending, ReviewDecision::Approved, 10),
                pr(6, "me", CheckState::None, ReviewDecision::Approved, 5),
            ],
            ..Default::default()
        }
    }

    fn numbers(g: &Grouped) -> Vec<(Group, Vec<u64>)> {
        g.groups.iter().map(|(g, l)| (*g, l.iter().map(|i| i.pr.pr.number).collect())).collect()
    }

    #[test]
    fn each_pull_request_goes_in_the_first_group_it_fits() {
        let g = group(&[&sample()], &InboxPrefs::default(), 0);
        assert_eq!(
            numbers(&g),
            [
                (Group::NeedsReview, vec![1]),
                (Group::Failing, vec![2]),
                (Group::ChangesRequested, vec![3]),
                (Group::Ready, vec![4, 6]),
                (Group::Mine, vec![5]),
            ],
            "failing beats changes requested; approved with checks running isn't ready yet; no checks at all is"
        );
        assert_eq!(g.action_count(), 5);
        let mut draft = pr(7, "me", CheckState::Passing, ReviewDecision::Approved, 1);
        draft.pr.state = PrState::Draft;
        assert_eq!(classify(&draft.pr, false), Group::Mine, "a draft isn't ready to merge");
        let mut conflicted = pr(8, "me", CheckState::Passing, ReviewDecision::Approved, 1);
        conflicted.pr.mergeable = Mergeable::Conflicts;
        assert_eq!(classify(&conflicted.pr, false), Group::Mine);
    }

    #[test]
    fn pinned_ones_go_first_and_snoozed_ones_aside() {
        let mut prefs = InboxPrefs::default();
        prefs.pin(PrRef { repo: key("acme/shop"), number: 5 });
        prefs.pin(PrRef { repo: key("acme/shop"), number: 3 });
        prefs.snooze(PrRef { repo: key("acme/shop"), number: 1 }, 1000);
        let g = group(&[&sample()], &prefs, 500);
        assert_eq!(numbers(&g)[0], (Group::Pinned, vec![5, 3]), "in pin order");
        assert_eq!(g.snoozed.iter().map(|i| i.pr.pr.number).collect::<Vec<_>>(), [1]);
        assert_eq!(g.action_count(), 4, "3 still counts where it belongs; the snoozed review doesn't");
        // Once the snooze is over, it's back.
        let later = group(&[&sample()], &prefs, 1000);
        assert!(later.snoozed.is_empty() && numbers(&later).iter().any(|(g, n)| *g == Group::NeedsReview && n == &[1]));
        // Snoozing a pinned one unpins it; pinning wakes it.
        prefs.snooze(PrRef { repo: key("acme/shop"), number: 5 }, 2000);
        assert_eq!(prefs.pinned, [PrRef { repo: key("acme/shop"), number: 3 }]);
        prefs.pin(PrRef { repo: key("acme/shop"), number: 5 });
        assert!(prefs.snoozed_until(&PrRef { repo: key("acme/shop"), number: 5 }, 0).is_none());
        // Gone from the inbox: forgotten.
        prefs.tidy(&[PrRef { repo: key("acme/shop"), number: 5 }], 0);
        assert_eq!((prefs.pinned.len(), prefs.snoozed.len()), (1, 0));
    }

    #[test]
    fn changes_alert_once_and_what_was_already_so_never() {
        let before = sample();
        assert!(alerts(&before, &before).is_empty(), "nothing changed");
        let mut after = sample();
        after.mine[2].pr.checks = CheckSummary::from_states([(CheckState::Failing, 1)]); // #4 fails
        after.mine[1].pr.review = ReviewDecision::Approved; // #3 approved
        after.review.push(pr(9, "jo", CheckState::Passing, ReviewDecision::ReviewRequired, 60));
        after.merged.push(pr(6, "me", CheckState::Passing, ReviewDecision::Approved, 70));
        // From a repository that wasn't asked about before: not news.
        after.review.push(InboxPr { repo: key("acme/new"), ..pr(10, "jo", CheckState::Passing, ReviewDecision::ReviewRequired, 60) });
        let got: Vec<(AlertKind, u64)> = alerts(&before, &after).iter().map(|a| (a.kind, a.pr.pr.number)).collect();
        assert_eq!(got, [(AlertKind::Approved, 3), (AlertKind::ChecksFailed, 4), (AlertKind::ReviewRequested, 9), (AlertKind::Merged, 6)]);
        assert!(alerts(&after, &after).is_empty(), "the next answer, the same: nothing again");
        // Failing already, still failing: nothing.
        let mut again = after.clone();
        again.mine[0].pr.updated += 1;
        assert!(alerts(&after, &again).is_empty());
        let a = &alerts(&before, &after)[3];
        assert_eq!(a.text(ForgeKind::GitHub), "#6 PR 6 was merged into main");
        assert_eq!(alerts(&before, &after)[2].text(ForgeKind::GitLab), "jo asks for your review on !9 PR 9");
    }
}
