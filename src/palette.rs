//! The command palette (Ctrl/Cmd+K, Ctrl/Cmd+Shift+P) and the keyboard cheat sheet (?).
//!
//! Type to search the actions, or start with a prefix to jump somewhere:
//! `@` repositories of the workspace, `#` branches, `:` commits (by id or message),
//! `/` files of the repository.

use crate::actions::{self, Ctx, Id};
use crate::app::Action;
use crate::fuzzy::{self, Match};
use crate::keys::{self, Bindings};
use crate::theme::*;
use crate::views;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use std::path::PathBuf;
use std::sync::Arc;

/// The most rows shown (the rest need a longer query).
const MAX_ROWS: usize = 100;
const ROW_H: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Actions,
    Repos,
    Branches,
    Commits,
    Files,
}

impl Mode {
    fn hint(self) -> &'static str {
        match self {
            Mode::Actions => "Type a command, or  @ repository  # branch  : commit  / file",
            Mode::Repos => "Go to a repository of this workspace",
            Mode::Branches => "Switch to a branch",
            Mode::Commits => "Find a commit by its id or message",
            Mode::Files => "Blame a file, or see its history",
        }
    }

    fn footer(self) -> &'static str {
        match self {
            Mode::Actions => "↑↓ move   Enter run   Esc close",
            Mode::Repos => "↑↓ move   Enter open   Esc close",
            Mode::Branches => "↑↓ move   Enter check out   Ctrl+Enter show in history   Esc close",
            Mode::Commits => "↑↓ move   Enter show in history   Esc close",
            Mode::Files => "↑↓ move   Enter blame (folders: history)   Ctrl+Enter history   Esc close",
        }
    }
}

/// Which mode a query asks for, and what's left of it.
pub fn mode_of(query: &str) -> (Mode, &str) {
    let q = query.trim_start();
    let mut chars = q.chars();
    match chars.next() {
        Some('@') => (Mode::Repos, chars.as_str()),
        Some('#') => (Mode::Branches, chars.as_str()),
        Some(':') => (Mode::Commits, chars.as_str()),
        Some('/') => (Mode::Files, chars.as_str()),
        Some('>') => (Mode::Actions, chars.as_str()),
        _ => (Mode::Actions, q),
    }
}

pub struct BranchItem {
    /// "main", or "origin/main" for a remote branch.
    pub name: String,
    pub remote: bool,
    pub head: bool,
    /// The worktree that has it checked out, when it's not this one.
    pub worktree: Option<PathBuf>,
    /// A remote branch that already has a local branch of the same name.
    pub has_local: bool,
    /// The commit it points at.
    pub tip: String,
}

pub struct CommitItem {
    pub hash: String,
    pub summary: String,
    pub author: String,
}

/// Everything the palette can jump to, read when it opens.
pub struct Sources {
    pub ctx: Ctx,
    /// Names of the actions run lately, newest first.
    pub recent: Vec<String>,
    pub repos: Vec<PathBuf>,
    pub branches: Vec<BranchItem>,
    pub commits: Vec<CommitItem>,
}

/// What a row does when picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Command(Id),
    Repo(PathBuf),
    Branch(usize),
    Commit(String),
    /// A file or folder (true) of the repository.
    File(String, bool),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub title: String,
    /// Which characters of the title the query matched.
    pub hits: Vec<usize>,
    /// Something dim after the title (a folder, a commit id, what a branch is).
    pub detail: String,
    pub enabled: bool,
    /// Why an unavailable action can't run now.
    pub why: Option<&'static str>,
    pub pick: Pick,
}

pub struct Found {
    pub mode: Mode,
    pub rows: Vec<Row>,
    /// The files are still being read.
    pub loading: bool,
    /// Why there's nothing to list (no repository is open).
    pub note: Option<&'static str>,
}

fn row(title: impl Into<String>, m: Option<Match>, detail: impl Into<String>, pick: Pick) -> Row {
    Row { title: title.into(), hits: m.map(|m| m.positions).unwrap_or_default(), detail: detail.into(), enabled: true, why: None, pick }
}

/// The rows for a query, best first.
pub fn search(src: &Sources, files: Option<&[(String, bool)]>, query: &str) -> Found {
    let (mode, q) = mode_of(query);
    let q = q.trim();
    let pat = fuzzy::Pattern::new(q);
    if src.ctx.repo.is_none() && matches!(mode, Mode::Branches | Mode::Commits | Mode::Files) {
        return Found { mode, rows: Vec::new(), loading: false, note: Some("Open a repository first") };
    }
    let mut scored: Vec<(i32, Row)> = Vec::new();
    let mut loading = false;
    match mode {
        Mode::Actions => {
            for (order, d) in actions::DEFS.iter().enumerate() {
                let by_title = pat.find(d.title);
                // Other words for the action count a little less than its title.
                let by_words = if d.also.is_empty() { None } else { pat.find(d.also).map(|m| Match { score: m.score - 25, positions: Vec::new() }) };
                let m = match (by_title, by_words) {
                    (Some(t), Some(w)) if w.score > t.score => Match { score: w.score, positions: t.positions },
                    (Some(t), _) => t,
                    (None, Some(w)) => w,
                    (None, None) => continue,
                };
                let recent = src.recent.iter().position(|k| k == d.key);
                let rank = if q.is_empty() { recent.map_or(0, |i| 1000 - i as i32) } else { m.score + recent.map_or(0, |i| 40 - 3 * (i.min(12) as i32)) };
                let enabled = actions::available(d.id, &src.ctx);
                let mut r = row(d.title, Some(m), d.group, Pick::Command(d.id));
                r.enabled = enabled;
                r.why = if enabled { None } else { actions::missing(d.id, &src.ctx) };
                // Unavailable ones last; then by rank, then in the table's order.
                scored.push((if enabled { rank } else { rank - 100_000 } * 1000 - order as i32, r));
            }
        }
        Mode::Repos => {
            for (i, p) in src.repos.iter().enumerate() {
                let name = crate::git::repo_name(p);
                let Some(m) = pat.find(&name) else { continue };
                let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
                scored.push((m.score * 1000 - i as i32, row(name, Some(m), dir, Pick::Repo(p.clone()))));
            }
        }
        Mode::Branches => {
            for (i, b) in src.branches.iter().enumerate() {
                let Some(m) = pat.find(&b.name) else { continue };
                let detail = if b.head {
                    "current".to_string()
                } else if b.worktree.is_some() {
                    "in another worktree".to_string()
                } else if b.remote {
                    "remote".to_string()
                } else {
                    String::new()
                };
                // Local branches before remote ones, the current one first.
                let kind = if b.head { 2 } else if b.remote { 0 } else { 1 };
                scored.push(((m.score + kind * 3) * 1000 - i as i32, row(b.name.clone(), Some(m), detail, Pick::Branch(i))));
            }
        }
        Mode::Commits => {
            let sha = q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit());
            for (i, c) in src.commits.iter().enumerate() {
                let short = &c.hash[..c.hash.len().min(8)];
                // The newest commits first when everything ties.
                let recency = -(i as i32).min(900);
                let m = if q.is_empty() {
                    Some(Match { score: 0, positions: Vec::new() })
                } else if sha && c.hash.starts_with(&q.to_ascii_lowercase()) {
                    Some(Match { score: 10_000, positions: Vec::new() })
                } else {
                    // The message, or its author at a lower weight.
                    let by_msg = pat.find(&c.summary);
                    let by_author = pat.find(&c.author).map(|m| Match { score: m.score - 40, positions: Vec::new() });
                    match (by_msg, by_author) {
                        (Some(a), Some(b)) if b.score > a.score => Some(Match { score: b.score, positions: a.positions }),
                        (Some(a), _) => Some(a),
                        (None, b) => b,
                    }
                };
                let Some(m) = m else { continue };
                scored.push(((m.score + recency) * 1000 - i as i32, row(c.summary.clone(), Some(m), short, Pick::Commit(c.hash.clone()))));
            }
        }
        Mode::Files => match files {
            None => loading = true,
            Some(all) => {
                for (i, (path, folder)) in all.iter().enumerate() {
                    if q.is_empty() && *folder {
                        continue;
                    }
                    let shown = if *folder { format!("{path}/") } else { path.clone() };
                    // Match the name first: "app" should find app.rs before src/apparatus/x.rs.
                    let name_start = shown.trim_end_matches('/').rfind('/').map_or(0, |n| n + 1);
                    let Some(mut m) = pat.find(&shown) else { continue };
                    if let Some(n) = pat.find(&shown[name_start..]) {
                        m.score += n.score / 2;
                    }
                    if *folder {
                        m.score -= 5;
                    }
                    scored.push((m.score * 1000 - (i as i32).min(900), row(shown, Some(m), "", Pick::File(path.clone(), *folder))));
                }
            }
        },
    }
    scored.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    let rows = scored.into_iter().take(MAX_ROWS).map(|(_, r)| r).collect();
    Found { mode, rows, loading, note: None }
}

/// What picking a row does. `alt` is the second choice (Ctrl/Cmd+Enter).
pub fn activate(pick: &Pick, alt: bool, src: &Sources) -> Vec<Action> {
    let repo = src.ctx.repo.as_deref();
    match pick {
        Pick::Command(id) => vec![Action::Recent(*id), Action::Run(*id)],
        Pick::Repo(p) => vec![Action::OpenRepo(p.clone())],
        // Show it in the history: clears a search, scrolls to it and puts a file view aside.
        Pick::Commit(h) => vec![Action::ShowCommit(h.clone())],
        Pick::File(path, folder) => {
            let (path, folder) = (path.clone(), *folder);
            vec![Action::File(if alt || folder {
                crate::app::FileOpen::History { path, folder }
            } else {
                crate::app::FileOpen::Blame { rev: None, path, line: None, back: false }
            })]
        }
        Pick::Branch(i) => {
            let (Some(b), Some(p)) = (src.branches.get(*i), repo) else { return Vec::new() };
            if alt || b.head {
                return vec![Action::ShowCommit(b.tip.clone())];
            }
            if let Some(w) = &b.worktree {
                // Git won't check out a branch twice: go to the worktree that has it.
                return vec![Action::OpenWorktree(w.clone())];
            }
            if b.remote {
                let local = b.name.split_once('/').map_or(b.name.as_str(), |(_, rest)| rest);
                vec![views::checkout_remote(p, &b.name, local, b.has_local)]
            } else {
                vec![views::checkout(p, &b.name)]
            }
        }
    }
}

/// The open palette.
pub struct Palette {
    pub query: String,
    picked: usize,
    pub src: Sources,
    /// Every file and folder, once read (asked for when `/` is first typed).
    pub files: Option<Result<Vec<(String, bool)>, String>>,
    files_asked: bool,
    found: Option<Found>,
    found_for: Option<(String, bool)>,
    /// The picked row changed by key: scroll to it.
    scroll: bool,
    bindings: Arc<Bindings>,
}

impl Palette {
    pub fn new(src: Sources, bindings: Arc<Bindings>, query: &str) -> Self {
        Self { query: query.into(), picked: 0, src, files: None, files_asked: false, found: None, found_for: None, scroll: false, bindings }
    }

    fn refresh(&mut self) {
        let key = (self.query.clone(), self.files.is_some());
        if self.found_for.as_ref() != Some(&key) {
            let files = self.files.as_ref().and_then(|f| f.as_ref().ok()).map(|v| v.as_slice());
            self.found = Some(search(&self.src, files, &self.query));
            self.found_for = Some(key);
            self.picked = 0;
            self.scroll = true;
        }
    }

    /// Draw it inside the dialog. Returns false once it should close.
    pub fn ui(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
        let ctx = ui.ctx().clone();
        ui.set_width(620.0);
        // Ctrl/Cmd+K again closes it.
        if keys::pressed_for(&ctx, Id::Palette) {
            return false;
        }
        self.refresh();
        let mode = self.found.as_ref().map_or(Mode::Actions, |f| f.mode);
        if mode == Mode::Files && !self.files_asked && self.src.ctx.repo.is_some() {
            self.files_asked = true;
            actions.push(Action::PaletteFiles);
        }
        // ↑/↓ move through the list, so the box keeps them (egui would otherwise move focus away).
        let keep_arrows = egui::EventFilter { vertical_arrows: true, ..Default::default() };
        let edit = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text(mode.hint()).desired_width(f32::INFINITY).event_filter(keep_arrows));
        edit.request_focus();
        edit.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Command palette"));
        if edit.changed() {
            self.refresh();
        }
        let n = self.found.as_ref().map_or(0, |f| f.rows.len());
        let (up, down, page_up, page_down, enter, alt_enter) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter),
            )
        });
        let key = [(up, views::NavKey::Up), (down, views::NavKey::Down), (page_up, views::NavKey::PageUp), (page_down, views::NavKey::PageDown)]
            .into_iter()
            .find(|(on, _)| *on)
            .map(|(_, k)| k);
        if let Some(to) = key.and_then(|k| views::nav_target(self.picked, n, k, 8)) {
            self.picked = to;
            self.scroll = true;
        }
        ui.add_space(6.0);

        let mut chosen: Option<(usize, bool)> = None;
        let Some(found) = &self.found else { return true };
        if found.loading || matches!(self.files, Some(Err(_))) && mode == Mode::Files {
            match &self.files {
                Some(Err(e)) => ui.label(RichText::new(format!("Couldn't list the files\n{e}")).color(WARN)),
                _ => ui.add(egui::Spinner::new().color(ACCENT)),
            };
        } else if let Some(note) = found.note {
            ui.label(RichText::new(note).color(FAINT));
        } else if found.rows.is_empty() {
            ui.label(RichText::new("Nothing matches").color(FAINT));
        }
        let scroll = std::mem::take(&mut self.scroll);
        egui::ScrollArea::vertical().max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, r) in found.rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, r.enabled, &r.title));
                let on = i == self.picked;
                if on || resp.hovered() {
                    ui.painter().rect_filled(rect, 4.0, if on { SELECT } else { HOVER });
                }
                if on && scroll {
                    resp.scroll_to_me(None);
                }
                let color = if r.enabled { TEXT } else { FAINT };
                let mut job = LayoutJob::default();
                for (k, ch) in r.title.chars().enumerate() {
                    let hit = r.hits.contains(&k);
                    job.append(&ch.to_string(), 0.0, TextFormat { font_id: theme_font(), color: if hit { ACCENT } else { color }, ..Default::default() });
                }
                let galley = ui.painter().layout_job(job);
                let y = rect.center().y - galley.size().y / 2.0;
                ui.painter().galley(egui::pos2(rect.left() + 10.0, y), galley.clone(), color);
                // The shortcut (or the detail) on the right, the group after the title.
                let right = if r.enabled && matches!(r.pick, Pick::Command(_)) {
                    match &r.pick {
                        Pick::Command(id) => self.bindings.hint(&ctx, *id),
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                if !right.is_empty() {
                    ui.painter().text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, &right, mono(12.0), DIM);
                } else if !r.detail.is_empty() && !matches!(r.pick, Pick::Command(_)) {
                    ui.painter().text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, &r.detail, prop(12.0), DIM);
                }
                if matches!(r.pick, Pick::Command(_)) {
                    let note = match r.why {
                        Some(why) if on || resp.hovered() => why.to_string(),
                        _ => r.detail.clone(),
                    };
                    let x = rect.left() + 10.0 + galley.size().x + 10.0;
                    if right.is_empty() || x + 120.0 < rect.right() - 90.0 {
                        ui.painter().text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, note, prop(11.0), FAINT);
                    }
                }
                if resp.clicked() {
                    chosen = Some((i, false));
                }
            }
        });
        if (enter || alt_enter) && n > 0 {
            chosen = Some((self.picked, alt_enter));
        }
        ui.add_space(6.0);
        ui.label(RichText::new(mode.footer()).small().color(FAINT));
        match chosen {
            Some((i, alt)) => {
                let row = &found.rows[i];
                if row.enabled {
                    actions.extend(activate(&row.pick, alt, &self.src));
                    false
                } else {
                    // A greyed-out action says why instead of doing nothing.
                    actions.push(Action::Run(match row.pick {
                        Pick::Command(id) => id,
                        _ => Id::Palette,
                    }));
                    false
                }
            }
            None => true,
        }
    }
}

fn theme_font() -> egui::FontId {
    prop(14.0)
}

/// The keyboard cheat sheet, made from the action table and the bindings in force.
pub fn cheat_sheet(ui: &mut Ui, bindings: &Bindings) -> bool {
    let ctx = ui.ctx().clone();
    ui.set_width(560.0);
    ui.heading("Keyboard shortcuts");
    ui.label(RichText::new("Change them in the “keybindings” section of the settings file.").color(DIM));
    ui.add_space(6.0);
    let mut open = true;
    egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
        let mut groups: Vec<&str> = Vec::new();
        for d in actions::DEFS {
            if !groups.contains(&d.group) {
                groups.push(d.group);
            }
        }
        for g in groups {
            ui.add_space(6.0);
            ui.label(RichText::new(g.to_uppercase()).small().color(DIM));
            for d in actions::DEFS.iter().filter(|d| d.group == g) {
                let shown: Vec<String> = bindings.of(d.id).iter().map(|s| s.show(&ctx)).collect();
                ui.horizontal(|ui| {
                    ui.label(d.title);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let text = if shown.is_empty() { "—".to_string() } else { shown.join("  or  ") };
                        ui.label(RichText::new(text).monospace().color(if shown.is_empty() { FAINT } else { TEXT }));
                    });
                });
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new("IN LISTS").small().color(DIM));
        for (what, key) in [
            ("Move", "↑ ↓"),
            ("Page up / down", "PageUp PageDown"),
            ("First / last", "Home End"),
            ("Open the row", "Enter"),
        ] {
            ui.horizontal(|ui| {
                ui.label(what);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(key).monospace()));
            });
        }
        ui.add_space(10.0);
        ui.label(RichText::new("IN THE PALETTE").small().color(DIM));
        for (what, key) in [("Repositories", "@"), ("Branches", "#"), ("Commits", ":"), ("Files", "/")] {
            ui.horizontal(|ui| {
                ui.label(what);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(key).monospace()));
            });
        }
    });
    ui.add_space(8.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button("Close").clicked() {
            open = false;
        }
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn src() -> Sources {
        let commit = |hash: &str, summary: &str, author: &str| CommitItem { hash: hash.into(), summary: summary.into(), author: author.into() };
        Sources {
            ctx: Ctx { repo: Some("/r/shop".into()), commits: true, remote: Some("origin".into()), branch: Some("main".into()), upstream: true, changes: 1, unstaged: 1, ..Default::default() },
            recent: Vec::new(),
            repos: vec!["/work/shop".into(), "/work/shipping-api".into(), "/home/me/dotfiles".into()],
            branches: vec![
                BranchItem { name: "main".into(), remote: false, head: true, worktree: None, has_local: false, tip: "aaaa".into() },
                BranchItem { name: "search-ready".into(), remote: false, head: false, worktree: None, has_local: false, tip: "bbbb".into() },
                BranchItem { name: "origin/feature/cart".into(), remote: true, head: false, worktree: None, has_local: false, tip: "cccc".into() },
                BranchItem { name: "origin/main".into(), remote: true, head: false, worktree: None, has_local: true, tip: "aaaa".into() },
                BranchItem { name: "docs".into(), remote: false, head: false, worktree: Some("/work/docs".into()), has_local: false, tip: "dddd".into() },
            ],
            commits: vec![
                commit("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", "Add search to the shop", "Ann"),
                commit("ffee00112233445566778899aabbccddeeff0011", "Fix the cart total", "Bob"),
                commit("0123456789abcdef0123456789abcdef01234567", "Start the cart", "Ann"),
            ],
        }
    }

    fn titles(f: &Found) -> Vec<&str> {
        f.rows.iter().map(|r| r.title.as_str()).collect()
    }

    #[test]
    fn prefixes_pick_the_mode() {
        assert_eq!(mode_of("push"), (Mode::Actions, "push"));
        assert_eq!(mode_of("@shop"), (Mode::Repos, "shop"));
        assert_eq!(mode_of("  #main"), (Mode::Branches, "main"));
        assert_eq!(mode_of(":fix"), (Mode::Commits, "fix"));
        assert_eq!(mode_of("/src"), (Mode::Files, "src"));
        assert_eq!(mode_of(">fetch"), (Mode::Actions, "fetch"));
        assert_eq!(mode_of(""), (Mode::Actions, ""));
    }

    #[test]
    fn actions_are_found_by_title_or_other_words() {
        let s = src();
        let f = search(&s, None, "stage");
        assert_eq!(f.rows[0].title, "Stage all changes");
        // "download" isn't in any title, but is one of Fetch's other words.
        let f = search(&s, None, "download");
        assert!(titles(&f).contains(&"Fetch"), "{:?}", titles(&f));
        assert!(search(&s, None, "zzzzqq").rows.is_empty());
    }

    #[test]
    fn recent_actions_come_first() {
        let mut s = src();
        s.recent = vec!["git.stash".into(), "view.find-file".into()];
        let f = search(&s, None, "");
        let first_two: Vec<&str> = f.rows.iter().take(2).map(|r| r.title.as_str()).collect();
        assert_eq!(first_two, ["Stash changes", "Find file…"], "newest first");
        // Among equal matches, the one used lately wins.
        let plain = search(&src(), None, "pu");
        assert_eq!(plain.rows[0].title, "Pull", "table order breaks the tie");
        s.recent = vec!["git.push".into()];
        let boosted = search(&s, None, "pu");
        assert_eq!(boosted.rows[0].title, "Push");
    }

    #[test]
    fn unavailable_actions_sink_and_say_why() {
        let s = src();
        let f = search(&s, None, "");
        let (on, off): (Vec<&Row>, Vec<&Row>) = f.rows.iter().partition(|r| r.enabled);
        assert!(!on.is_empty() && !off.is_empty());
        let last_on = f.rows.iter().rposition(|r| r.enabled).unwrap();
        let first_off = f.rows.iter().position(|r| !r.enabled).unwrap();
        assert!(last_on < first_off, "available ones first");
        let commit = f.rows.iter().find(|r| r.title == "Commit").unwrap();
        assert!(!commit.enabled && commit.why.is_some());
    }

    #[test]
    fn repos_branches_and_commits_have_their_prefixes() {
        let s = src();
        assert_eq!(titles(&search(&s, None, "@sh")), ["shop", "shipping-api"]);
        assert_eq!(titles(&search(&s, None, "@dot")), ["dotfiles"]);
        let f = search(&s, None, "#main");
        assert_eq!(f.mode, Mode::Branches);
        assert_eq!(titles(&f)[0], "main", "the current branch first");
        assert!(titles(&f).contains(&"origin/main"));
        assert_eq!(titles(&search(&s, None, "#cart")), ["origin/feature/cart"]);
        // By message, by id, and by author.
        assert_eq!(titles(&search(&s, None, ":cart")), ["Fix the cart total", "Start the cart"]);
        let by_id = search(&s, None, ":ffee00");
        assert_eq!(titles(&by_id), ["Fix the cart total"]);
        assert_eq!(by_id.rows[0].detail, "ffee0011");
        assert_eq!(titles(&search(&s, None, ":bob")), ["Fix the cart total"]);
        assert_eq!(search(&s, None, ":").rows.len(), 3, "an empty query lists the newest commits");
    }

    #[test]
    fn files_wait_for_the_list_then_match_names_first() {
        let s = src();
        assert!(search(&s, None, "/app").loading);
        let files: Vec<(String, bool)> = [("src/apparatus/x.rs", false), ("src/app.rs", false), ("src", true), ("docs/app-guide.md", false)].iter().map(|(p, f)| (p.to_string(), *f)).collect();
        let f = search(&s, Some(&files), "/app");
        assert!(!f.loading);
        assert_eq!(f.rows[0].title, "src/app.rs", "{:?}", titles(&f));
        let all = search(&s, Some(&files), "/");
        assert!(all.rows.iter().all(|r| !matches!(r.pick, Pick::File(_, true))), "no folders until you type");
    }

    #[test]
    fn picking_a_branch_does_the_right_checkout() {
        let s = src();
        let one = |p: Pick, alt: bool| activate(&p, alt, &s);
        // A local branch: check it out.
        assert!(matches!(one(Pick::Branch(1), false).as_slice(), [Action::Git { label, .. }] if label == "Switch to search-ready"));
        // The one already checked out, or Ctrl+Enter: show it in the history.
        assert!(matches!(one(Pick::Branch(0), false).as_slice(), [Action::ShowCommit(h)] if h == "aaaa"));
        assert!(matches!(one(Pick::Branch(1), true).as_slice(), [Action::ShowCommit(h)] if h == "bbbb"));
        // A remote branch without a local one is checked out tracking it; with one, the local is.
        assert!(matches!(one(Pick::Branch(2), false).as_slice(), [Action::Git { label, .. }] if label == "Check out origin/feature/cart"));
        assert!(matches!(one(Pick::Branch(3), false).as_slice(), [Action::Git { label, .. }] if label == "Switch to main"));
        // Another worktree has it: go there (git won't check it out twice).
        assert!(matches!(one(Pick::Branch(4), false).as_slice(), [Action::OpenWorktree(p)] if p == Path::new("/work/docs")));
        assert!(matches!(one(Pick::Commit("abc".into()), false).as_slice(), [Action::ShowCommit(h)] if h == "abc"));
        assert!(matches!(one(Pick::File("a.rs".into(), false), false).as_slice(), [Action::File(crate::app::FileOpen::Blame { .. })]));
        assert!(matches!(one(Pick::File("a.rs".into(), false), true).as_slice(), [Action::File(crate::app::FileOpen::History { .. })]));
        assert!(matches!(one(Pick::Command(Id::Fetch), false).as_slice(), [Action::Recent(Id::Fetch), Action::Run(Id::Fetch)]), "picked here: remembered, then run");
    }
}
