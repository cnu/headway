//! The repository screen: toolbar, sidebar, history, commit details, staging and diffs.

use crate::app::{Action, DiffDoc, Dialog, FileOpen, FileSel, FlatRow, FollowUp, LineSel, OpKind, Record, RepoData, RepoView, Selection, Summary};
use crate::git::{self, Change, FileChange, LineKind, RefKind};
use crate::graph::Target;
use crate::preview;
use crate::theme::{self, *};
use crate::workspace::{ViewPrefs, Workspace};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Align2, Color32, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn repo(
    ui: &mut Ui,
    rv: &mut RepoView,
    ws: &Workspace,
    summaries: &HashMap<PathBuf, Result<Summary, String>>,
    busy: &[String],
    view: &mut ViewPrefs,
    actions: &mut Vec<Action>,
) {
    if !rv.modal && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::B)) {
        view.sidebar_open = !view.sidebar_open;
    }
    egui::Panel::top("repo_toolbar")
        .exact_size(46.0)
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| toolbar(ui, rv, busy, &mut view.sidebar_open, actions));
    crate::resolver::banner(ui, rv, actions);
    crate::fileview::banner(ui, rv, actions);

    egui::Panel::left("sidebar")
        .default_size(210.0)
        .size_range(160.0..=420.0)
        .resizable(true)
        .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(Margin::symmetric(8, 8)))
        .show_collapsible(ui, &mut view.sidebar_open, |ui| sidebar(ui, rv, ws, summaries, actions));

    if rv.data.is_none() {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(PANEL)).show(ui, |ui| {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| match &rv.error {
                Some(e) => {
                    ui.label(RichText::new("⚠").size(32.0).color(WARN));
                    ui.label(RichText::new(e).color(TEXT));
                    ui.add_space(8.0);
                    if ui.button("Try again").clicked() {
                        actions.push(Action::Refresh);
                    }
                }
                None => {
                    ui.add(egui::Spinner::new().size(24.0).color(ACCENT));
                    ui.label(RichText::new("Reading repository…").color(DIM));
                }
            });
        });
        return;
    }

    // A conflicted file being resolved takes the whole width.
    if rv.resolver.as_ref().is_some_and(|r| r.shown) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| crate::resolver::show(ui, rv, actions));
        return;
    }

    // So do blame and file history.
    if rv.file_view.as_ref().is_some_and(|f| f.shown) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| crate::fileview::show(ui, rv, view, actions));
        return;
    }

    // History and details share the width by a remembered fraction (half by default),
    // so the split holds when the window is resized.
    let total = ui.available_width();
    let min_frac = (260.0 / total).min(0.45);
    view.split = view.split.clamp(min_frac, 1.0 - min_frac);
    let detail = egui::Panel::right("detail")
        .exact_size((total * view.split).round())
        .resizable(false)
        .frame(egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| match rv.selection.clone() {
            Some(Selection::WorkingDir) | None => staging_panel(ui, rv, actions),
            Some(Selection::Commit(_)) => commit_panel(ui, rv, actions),
            Some(Selection::Reflog) => reflog_panel(ui, rv, actions),
        });
    let edge = detail.response.rect.left();

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| commit_list(ui, rv, actions));

    // Draggable divider between the two (added last so it wins over the list underneath).
    let handle = Rect::from_x_y_ranges(edge - 4.0..=edge + 4.0, detail.response.rect.y_range());
    let h = ui.interact(handle, ui.scope_id().with("split_handle"), Sense::click_and_drag());
    name_it(&h, egui::Role::Splitter, "Resize history and details");
    if h.hovered() || h.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().line_segment([pos2(edge, handle.top()), pos2(edge, handle.bottom())], Stroke::new(2.0, ACCENT));
    }
    if h.dragged() && total > 0.0 {
        view.split = (view.split - h.drag_delta().x / total).clamp(min_frac, 1.0 - min_frac);
    }
    if h.double_clicked() {
        view.split = 0.5;
    }
    h.on_hover_text("Drag to resize · double-click to split in half");
}

// ---------------------------------------------------------------------------
// Toolbar

fn toolbar(ui: &mut Ui, rv: &mut RepoView, busy: &[String], sidebar_open: &mut bool, actions: &mut Vec<Action>) {
    let path = rv.path.clone();
    let search_id = egui::Id::unique(("search", &path));
    if !rv.modal && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
        ui.memory_mut(|m| m.request_focus(search_id));
    }
    ui.horizontal_centered(|ui| {
        let tip = if *sidebar_open { "Hide the sidebar (Ctrl+B)" } else { "Show the sidebar (Ctrl+B)" };
        let toggle = egui::Button::new(RichText::new("◧").size(15.0)).selected(*sidebar_open);
        if ui.add(toggle).on_hover_text(tip).clicked() {
            *sidebar_open = !*sidebar_open;
        }
        ui.label(RichText::new(git::repo_name(&path)).size(16.0).strong());
        let Some(data) = &rv.data else { return };
        let head = &data.status.head;
        let current = head.branch.clone().unwrap_or_else(|| "detached".into());
        let switcher = egui::ComboBox::from_id_salt(("branch_switch", &path))
            .selected_text(RichText::new(format!("⎇  {current}")).color(TEXT))
            .width(170.0)
            .show_ui(ui, |ui| {
                ui.label(RichText::new("SWITCH TO BRANCH").small().color(DIM));
                let elsewhere = branches_in_other_worktrees(data);
                for b in data.branches.iter().filter(|b| !b.remote) {
                    let other = elsewhere.get(b.name.as_str());
                    let text = if other.is_some() { format!("{}   ⧉", b.name) } else { b.name.clone() };
                    let r = ui.selectable_label(b.is_head, text);
                    if r.clicked() && !b.is_head {
                        actions.push(match other {
                            // Git won't check out a branch twice: go to the worktree that has it.
                            Some(p) => Action::OpenWorktree(p.to_path_buf()),
                            None => checkout(&path, &b.name),
                        });
                    }
                    if let Some(p) = other {
                        r.on_hover_text(format!("Checked out in the worktree at {}\nClick to open it", p.display()));
                    }
                }
                ui.separator();
                if ui.button("New branch…").clicked() {
                    actions.push(new_branch(&path, "HEAD", &format!("the current branch ({current})")));
                }
                if ui.button("New worktree…").on_hover_text("Work on another branch in a second folder").clicked() {
                    actions.push(new_worktree(&path, data, true, "", "HEAD", &format!("the current branch ({current})")));
                }
            });
        let branch_label = format!("Current branch: {current}");
        switcher.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &branch_label));
        ui.add_space(6.0);

        let has_remote = !data.remotes.is_empty();
        let changes = data.status.change_count();
        if tool_button(ui, "⇅", "Fetch", None, has_remote, "Download news from the remote without changing your files").clicked() {
            actions.push(Action::args(&path, "Fetch", &["fetch", "--all", "--prune"]));
        }
        let behind = (head.behind > 0).then(|| head.behind.to_string());
        if tool_button(ui, "⬇", "Pull", behind, has_remote && head.upstream.is_some(), "Bring in new commits from the remote branch").clicked() {
            actions.push(Action::args(&path, "Pull", &["-c", "pull.rebase=false", "pull", "--no-edit"]).undoable(git::Undoable::Tree { save_work: false }, "pull"));
        }
        let ahead = (head.ahead > 0).then(|| head.ahead.to_string());
        let push_tip = if head.upstream.is_some() { "Send your commits to the remote branch" } else { "Publish this branch to the remote" };
        if tool_button(ui, "⬆", "Push", ahead, has_remote && head.branch.is_some(), push_tip).clicked() {
            actions.push(push_action(&path, head.branch.as_deref(), head.upstream.is_some(), data.remotes.first()));
        }
        sep(ui);
        if tool_button(ui, "⤓", "Stash", None, changes > 0, "Put your uncommitted changes aside for later").clicked() {
            actions.push(Action::args(&path, "Stash changes", &["stash", "push", "--include-untracked"]));
        }
        if tool_button(ui, "⤒", "Pop", None, !data.stashes.is_empty(), "Bring back the most recently stashed changes").clicked() {
            actions.push(Action::args(&path, "Restore stash", &["stash", "pop"]));
        }
        if tool_button(ui, "⑂", "Branch", None, !data.commits.is_empty(), "Create a new branch from here").clicked() {
            actions.push(new_branch(&path, "HEAD", &format!("the current branch ({current})")));
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let menu = ui.menu_button(RichText::new("⋯").size(15.0).color(DIM), |ui| repo_menu(ui, &path, data, actions));
            name_it(&menu.response, egui::Role::Button, "Repository menu");
            if !busy.is_empty() {
                ui.add(egui::Spinner::new().size(14.0).color(ACCENT)).on_hover_text(busy.join("\n"));
            }
            // Escape in the search box clears it. The box drops focus as soon as Escape
            // arrives, so remember whether it had focus on the previous frame.
            let focus_key = search_id.with("had_focus");
            let had_focus = ui.data(|d| d.get_temp::<bool>(focus_key)).unwrap_or(false);
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let te = egui::TextEdit::singleline(&mut rv.search)
                .id(search_id)
                .hint_text("🔍 Search commits  (Ctrl+F)")
                .desired_width(220.0);
            let r = ui.add(te);
            if had_focus && escape {
                rv.search.clear();
            }
            let focused_now = r.has_focus();
            ui.data_mut(|d| d.insert_temp(focus_key, focused_now));
        });
    });
}

/// The ⋯ menu at the right of the toolbar.
fn repo_menu(ui: &mut Ui, path: &Path, data: &RepoData, actions: &mut Vec<Action>) {
    ui.set_min_width(240.0);
    let undo_key = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z));
    let text = match &data.undo {
        Some(e) => format!("Undo: {}", e.what),
        None => "Undo".to_string(),
    };
    let tip = match &data.undo {
        Some(e) => format!("Done {}", theme::relative(e.time)),
        None => "Nothing to undo yet".to_string(),
    };
    let undo = ui.add_enabled(data.undo.is_some(), egui::Button::new(text).shortcut_text(undo_key)).on_hover_text(&tip).on_disabled_hover_text(&tip);
    if undo.clicked() {
        if let Some(e) = &data.undo {
            actions.push(Action::Undo { repo: path.to_path_buf(), id: Some(e.id) });
        }
        ui.close();
    }
    if ui.button("Reflog").on_hover_text("Everywhere HEAD and your branches have been").clicked() {
        actions.push(Action::ShowReflog(None));
        ui.close();
    }
    let find_key = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::P));
    if ui.add(egui::Button::new("Find file…").shortcut_text(find_key)).on_hover_text("Blame a file, or see the history of a file or folder").clicked() {
        actions.push(Action::FindFile);
        ui.close();
    }
    ui.separator();
    if ui.button("Copy folder path").clicked() {
        actions.push(Action::Copy(path.display().to_string()));
        ui.close();
    }
}

fn tool_button(ui: &mut Ui, icon: &str, label: &str, badge: Option<String>, enabled: bool, tip: &str) -> egui::Response {
    let text = match &badge {
        Some(b) => format!("{icon}  {label}  {b}"),
        None => format!("{icon}  {label}"),
    };
    let mut btn = egui::Button::new(RichText::new(text).size(13.0));
    if badge.is_some() {
        btn = btn.fill(Color32::from_rgb(54, 46, 78));
    }
    ui.add_enabled(enabled, btn).on_hover_text(tip).on_disabled_hover_text(tip)
}

fn sep(ui: &mut Ui) {
    ui.add_space(2.0);
    let (r, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, BORDER);
    ui.add_space(2.0);
}

// ---------------------------------------------------------------------------
// Action builders

fn checkout(path: &Path, name: &str) -> Action {
    Action::args(path, format!("Switch to {name}"), &["checkout", name]).undoable(git::Undoable::Tree { save_work: false }, format!("switch to {name}"))
}

fn new_branch(path: &Path, at: &str, label: &str) -> Action {
    Action::Dialog(Dialog::CreateBranch {
        repo: path.to_path_buf(),
        at: at.into(),
        label: label.into(),
        name: String::new(),
        checkout: true,
    })
}

/// Local branches checked out in some other worktree, with that worktree's folder.
fn branches_in_other_worktrees(data: &RepoData) -> HashMap<&str, &Path> {
    data.worktrees
        .iter()
        .filter(|w| !w.is_current)
        .filter_map(|w| Some((w.branch.as_deref()?, w.path.as_path())))
        .collect()
}

/// Open the "new worktree" dialog.
fn new_worktree(path: &Path, data: &RepoData, new_branch: bool, existing: &str, start: &str, start_label: &str) -> Action {
    let taken: std::collections::HashSet<&str> = data.worktrees.iter().filter_map(|w| w.branch.as_deref()).collect();
    let free_branches: Vec<String> =
        data.branches.iter().filter(|b| !b.remote && !taken.contains(b.name.as_str())).map(|b| b.name.clone()).collect();
    Action::Dialog(Dialog::CreateWorktree {
        repo: path.to_path_buf(),
        free_branches,
        new_branch,
        existing: existing.to_string(),
        name: String::new(),
        start: start.to_string(),
        start_label: start_label.to_string(),
        path: String::new(),
        path_edited: false,
    })
}

fn push_action(path: &Path, branch: Option<&str>, has_upstream: bool, remote: Option<&String>) -> Action {
    if has_upstream {
        Action::args(path, "Push", &["push"])
    } else {
        let b = branch.unwrap_or("HEAD").to_string();
        let r = remote.cloned().unwrap_or_else(|| "origin".into());
        Action::args(path, format!("Publish {b}"), &["push", "-u", &r, &b])
    }
}

// ---------------------------------------------------------------------------
// Sidebar

fn section(ui: &mut Ui, id: &str, title: &str, count: usize, open: bool, add: impl FnOnce(&mut Ui)) {
    let text = if count > 0 { format!("{title}   {count}") } else { title.to_string() };
    egui::CollapsingHeader::new(RichText::new(text).size(11.5).strong().color(DIM))
        .id_salt(id)
        .default_open(open)
        .show_unindented(ui, add);
    ui.add_space(4.0);
}

fn side_row(ui: &mut Ui, icon: &str, icon_color: Color32, text: &str, right: Option<(String, Color32)>, strong: bool) -> egui::Response {
    side_row_ex(ui, icon, icon_color, text, right, strong, None).0
}

/// Give a hand-drawn control a name (for screen readers and tests).
pub(crate) fn name_it(resp: &egui::Response, role: egui::Role, label: impl ToString) {
    let label = label.to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(role, resp.enabled(), &label));
}

/// The show/hide toggle on a branch row.
#[derive(Clone, Copy)]
struct Eye {
    hidden: bool,
    /// The current branch can't be hidden.
    enabled: bool,
}

/// A sidebar row, optionally with an eye toggle at the right. Returns the row and
/// whether the eye was clicked.
fn side_row_ex(
    ui: &mut Ui,
    icon: &str,
    icon_color: Color32,
    text: &str,
    right: Option<(String, Color32)>,
    strong: bool,
    eye: Option<Eye>,
) -> (egui::Response, bool) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 23.0), Sense::click());
    name_it(&resp, egui::Role::Button, text);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, HOVER);
    }
    let y = rect.center().y;
    p.text(pos2(rect.left() + 12.0, y), Align2::CENTER_CENTER, icon, theme::prop(12.0), icon_color);
    let font = theme::prop(13.0);
    let hidden = eye.is_some_and(|e| e.hidden);
    let color = if hidden {
        FAINT
    } else if strong {
        TEXT
    } else {
        Color32::from_rgb(190, 196, 205)
    };
    let eye_w = if eye.is_some() { 22.0 } else { 0.0 };
    let rect = Rect::from_min_max(rect.min, pos2(rect.right() - eye_w, rect.bottom()));
    // Space after the icon, shared by the name and the right-hand note. The name keeps
    // its full width when it fits; the note never takes more than what's left, and
    // always at least ~40% so it stays readable. Both are cut with "…" if needed.
    let space = (rect.width() - 30.0).max(20.0);
    let mut right_w = 0.0;
    if let Some((rt, rc)) = right {
        let name_w = p.layout_no_wrap(text.to_string(), font.clone(), color).size().x;
        let max_right = (space - name_w - 12.0).max(space * 0.4);
        let g = trunc(p, &rt, theme::prop(11.5), rc, max_right);
        right_w = g.size().x + 10.0;
        p.galley(pos2(rect.right() - g.size().x - 4.0, y - g.size().y / 2.0), g, rc);
    }
    let g = trunc(p, text, font, color, space - right_w);
    p.galley(pos2(rect.left() + 26.0, y - g.size().y / 2.0), g, color);
    if strong {
        p.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 4.0), vec2(2.0, 15.0)), 1.0, ACCENT);
    }
    let mut eye_clicked = false;
    if let Some(e) = eye {
        // Its own click area, added after the row so it sits on top.
        let er = Rect::from_center_size(pos2(rect.right() + eye_w / 2.0, y), vec2(20.0, 20.0));
        let eresp = ui.interact(er, resp.id.with("eye"), if e.enabled { Sense::click() } else { Sense::hover() });
        let eye_label = format!("{} {text}", if e.hidden { "Show" } else { "Hide" });
        // The current branch's eye can't be used; say so to screen readers too.
        eresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, e.enabled, &eye_label));
        let p = ui.painter();
        if eresp.hovered() && e.enabled {
            p.rect_filled(er, 4.0, RAISED);
        }
        // Visible branches show the eye faintly until the row is hovered; hidden ones always show it.
        let c = match (e.enabled, e.hidden, eresp.hovered(), resp.hovered()) {
            (false, ..) => FAINT.gamma_multiply(0.5),
            (_, _, true, _) => TEXT,
            (_, true, _, _) => WARN,
            (_, false, _, true) => DIM,
            _ => FAINT.gamma_multiply(0.7),
        };
        paint_eye(p, er.center(), c, e.hidden);
        let tip = match (e.enabled, e.hidden) {
            (false, _) => "The current branch is always shown",
            (true, true) => "Hidden from the history · click to show",
            (true, false) => "Click to hide this branch and its own commits from the history",
        };
        eye_clicked = eresp.on_hover_text(tip).clicked() && e.enabled;
    }
    (resp, eye_clicked)
}

/// A small triangle: pointing down when `open`, right when folded.
fn paint_chevron(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![pos2(c.x - 4.0, c.y - 2.0), pos2(c.x + 4.0, c.y - 2.0), pos2(c.x, c.y + 3.0)]
    } else {
        vec![pos2(c.x - 2.0, c.y - 4.0), pos2(c.x + 3.0, c.y), pos2(c.x - 2.0, c.y + 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// An eye drawn with lines (with a slash when `slashed`), so it looks the same everywhere.
fn paint_eye(p: &egui::Painter, c: Pos2, color: Color32, slashed: bool) {
    let (w, h) = (7.0, 4.2);
    let n = 12;
    let mut pts = Vec::with_capacity(2 * n);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        pts.push(pos2(c.x - w + 2.0 * w * t, c.y - h * (std::f32::consts::PI * t).sin()));
    }
    for i in 1..n {
        let t = 1.0 - i as f32 / n as f32;
        pts.push(pos2(c.x - w + 2.0 * w * t, c.y + h * (std::f32::consts::PI * t).sin()));
    }
    p.add(egui::Shape::closed_line(pts, Stroke::new(1.3, color)));
    p.circle_filled(c, 2.0, color);
    if slashed {
        p.line_segment([pos2(c.x - 6.0, c.y + 5.5), pos2(c.x + 6.0, c.y - 5.5)], Stroke::new(3.0, SIDEBAR));
        p.line_segment([pos2(c.x - 6.0, c.y + 5.5), pos2(c.x + 6.0, c.y - 5.5)], Stroke::new(1.3, color));
    }
}

fn sidebar(
    ui: &mut Ui,
    rv: &mut RepoView,
    ws: &Workspace,
    summaries: &HashMap<PathBuf, Result<Summary, String>>,
    actions: &mut Vec<Action>,
) {
    let path = rv.path.clone();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        section(ui, "ws", &format!("WORKSPACE · {}", ws.name.to_uppercase()), 0, true, |ui| {
            for (i, r) in ws.repos.iter().enumerate() {
                let right = match summaries.get(r) {
                    Some(Ok(s)) => {
                        let n = s.status.change_count();
                        let mut t = String::new();
                        if s.status.head.ahead > 0 {
                            t += &format!("↑{} ", s.status.head.ahead);
                        }
                        if s.status.head.behind > 0 {
                            t += &format!("↓{} ", s.status.head.behind);
                        }
                        if n > 0 {
                            t += &format!("●{n}");
                        }
                        (!t.is_empty()).then(|| (t, if n > 0 { WARN } else { PURPLE }))
                    }
                    Some(Err(_)) => Some(("⚠".into(), WARN)),
                    None => None,
                };
                let resp = side_row(ui, "■", theme::lane(i), &git::repo_name(r), right, *r == path);
                if resp.clicked() && *r != path {
                    actions.push(Action::OpenRepo(r.clone()));
                }
                resp.on_hover_text(r.display().to_string());
            }
        });

        let Some(data) = &rv.data else { return };
        let current = data.status.head.branch.clone().unwrap_or_default();

        let reflog_open = rv.selection == Some(Selection::Reflog);
        let resp = side_row(ui, "↺", PURPLE, "Reflog", None, reflog_open);
        if resp.clicked() {
            actions.push(Action::ShowReflog(None));
        }
        resp.on_hover_text("Everywhere HEAD and your branches have been, to find lost commits");
        ui.add_space(4.0);

        let locals: Vec<_> = data.branches.iter().filter(|b| !b.remote).collect();
        let elsewhere = branches_in_other_worktrees(data);
        section(ui, "branches", "BRANCHES", locals.len(), true, |ui| {
            for b in &locals {
                let other = elsewhere.get(b.name.as_str()).map(|p| p.to_path_buf());
                let right = (!b.track.is_empty() && b.track != "gone").then(|| {
                    let t = b.track.replace("ahead ", "↑").replace("behind ", "↓").replace(", ", " ");
                    (t, PURPLE)
                });
                let (icon, icon_color) = match (&other, b.is_head) {
                    (_, true) => ("✔", ACCENT),
                    (Some(_), _) => ("⧉", BLUE),
                    _ => ("⎇", DIM),
                };
                let full_ref = format!("refs/heads/{}", b.name);
                let eye = Eye { hidden: data.hidden.contains(&full_ref), enabled: !b.is_head };
                let (resp, eye_clicked) = side_row_ex(ui, icon, icon_color, &b.name, right, b.is_head, Some(eye));
                if eye_clicked {
                    actions.push(Action::ToggleHidden(full_ref));
                }
                if resp.double_clicked() && !b.is_head {
                    actions.push(match &other {
                        Some(p) => Action::OpenWorktree(p.clone()),
                        None => checkout(&path, &b.name),
                    });
                } else if resp.clicked() {
                    actions.push(Action::SelectRef(b.name.clone()));
                }
                let name = b.name.clone();
                let mut tip = name.clone();
                if let Some(u) = &b.upstream {
                    tip += &format!("\nTracks {u}");
                }
                tip += &match &other {
                    Some(p) => format!("\nChecked out in the worktree at {}\nDouble-click to open it", p.display()),
                    None => "\nDouble-click to switch to it".to_string(),
                };
                resp.on_hover_text(tip).context_menu(|ui| {
                    if let Some(p) = &other {
                        if ui.button("Open its worktree").clicked() {
                            actions.push(Action::OpenWorktree(p.clone()));
                        }
                    }
                    if ui.add_enabled(!b.is_head && other.is_none(), egui::Button::new(format!("Switch to {name}"))).clicked() {
                        actions.push(checkout(&path, &name));
                    }
                    if !b.is_head && other.is_none() && ui.button(format!("New worktree for {name}…")).clicked() {
                        actions.push(new_worktree(&path, data, false, &name, "", ""));
                    }
                    if ui.add_enabled(!b.is_head && !current.is_empty(), egui::Button::new(format!("Merge {name} into {current}"))).clicked() {
                        actions.push(Action::confirm(
                            "Merge branch",
                            format!("Bring the changes from “{name}” into “{current}”?"),
                            "Merge",
                            false,
                            Action::args(&path, format!("Merge {name}"), &["merge", "--no-edit", &name]).undoable(git::Undoable::Tree { save_work: false }, format!("merge {name}")),
                        ));
                    }
                    if ui.add_enabled(!b.is_head && !current.is_empty(), egui::Button::new(format!("Rebase {current} onto {name}"))).clicked() {
                        actions.push(Action::confirm(
                            "Rebase branch",
                            format!("Replay the commits of “{current}” on top of “{name}”? This rewrites the history of “{current}”."),
                            "Rebase",
                            false,
                            Action::args(&path, format!("Rebase onto {name}"), &["rebase", &name]).undoable(git::Undoable::Tree { save_work: false }, format!("rebase onto {name}")),
                        ));
                    }
                    if ui.button("New branch from here…").clicked() {
                        actions.push(new_branch(&path, &name, &name));
                    }
                    if ui.button("Show reflog").on_hover_text("Everywhere this branch has pointed").clicked() {
                        actions.push(Action::ShowReflog(Some(format!("refs/heads/{name}"))));
                    }
                    if !data.remotes.is_empty() && ui.button("Push").clicked() {
                        actions.push(push_action(&path, Some(&name), b.upstream.is_some(), data.remotes.first()));
                    }
                    ui.separator();
                    if ui.button("Copy name").clicked() {
                        actions.push(Action::Copy(name.clone()));
                    }
                    if ui.add_enabled(!b.is_head, egui::Button::new(RichText::new("Delete…").color(DANGER))).clicked() {
                        actions.push(Action::confirm(
                            "Delete branch",
                            format!("Delete the branch “{name}”? The commits stay if another branch contains them."),
                            "Delete",
                            true,
                            Action::args(&path, format!("Delete {name}"), &["branch", "-d", &name]).undoable(git::Undoable::Refs, format!("delete branch {name}")),
                        ));
                    }
                    if ui.add_enabled(!b.is_head, egui::Button::new(RichText::new("Force delete…").color(DANGER))).clicked() {
                        actions.push(Action::confirm(
                            "Force delete branch",
                            format!("Delete “{name}” even if its commits aren't merged anywhere? They may be lost."),
                            "Force delete",
                            true,
                            Action::args(&path, format!("Delete {name}"), &["branch", "-D", &name]).undoable(git::Undoable::Refs, format!("delete branch {name}")),
                        ));
                    }
                });
            }
        });

        // Hidden branches that still exist.
        let hidden_count = data
            .branches
            .iter()
            .filter(|b| {
                let full = if b.remote { format!("refs/remotes/{}", b.name) } else { format!("refs/heads/{}", b.name) };
                data.hidden.contains(&full)
            })
            .count();
        if hidden_count > 0 {
            let text = format!("Show {hidden_count} hidden branch{}", if hidden_count == 1 { "" } else { "es" });
            if side_row(ui, "◉", WARN, &text, None, false).on_hover_text("Put every hidden branch back in the history").clicked() {
                actions.push(Action::ShowAllBranches);
            }
            ui.add_space(4.0);
        }

        let shown: Vec<_> = data.worktrees.iter().filter(|w| !w.bare).collect();
        section(ui, "worktrees", "WORKTREES", shown.len(), shown.len() > 1, |ui| {
            let main_path = shown.iter().find(|w| w.is_main).map(|w| w.path.clone());
            for w in &shown {
                let folder = git::repo_name(&w.path);
                // Folders named "<project>-<branch>" (the usual way) say the branch already:
                // show just the branch part and skip the repeated note.
                let usual_name = match (&main_path, &w.branch) {
                    (Some(m), Some(b)) if !w.is_main => git::default_worktree_path(m, b).file_name().is_some_and(|f| f.to_string_lossy() == folder),
                    _ => false,
                };
                let label = match (&w.branch, usual_name) {
                    (Some(b), true) => b.clone(),
                    _ => folder.clone(),
                };
                let right = if w.prunable {
                    Some(("missing".to_string(), WARN))
                } else if usual_name {
                    None
                } else {
                    Some((w.branch.clone().unwrap_or_else(|| format!("detached {}", &w.head[..7.min(w.head.len())])), DIM))
                };
                let icon = if w.is_current { "✔" } else { "⧉" };
                let resp = side_row(ui, icon, if w.is_current { ACCENT } else { BLUE }, &label, right, w.is_current);
                if resp.clicked() && !w.is_current && !w.prunable {
                    actions.push(Action::OpenWorktree(w.path.clone()));
                }
                let mut tip = w.path.display().to_string();
                match &w.branch {
                    Some(b) => tip += &format!("\nBranch: {b}"),
                    None if !w.head.is_empty() => tip += &format!("\nNo branch (at commit {})", &w.head[..8.min(w.head.len())]),
                    None => {}
                }
                if w.is_main {
                    tip += "\nThe repository's main folder";
                }
                if w.is_current {
                    tip += "\nYou're looking at this one";
                } else if !w.prunable {
                    tip += "\nClick to open it";
                }
                if w.locked {
                    tip += "\nLocked: git won't remove it";
                }
                if w.prunable {
                    tip += "\nIts folder is gone; use “Clean up missing worktrees”";
                }
                let wp = w.path.clone();
                let removable = !w.is_main && !w.is_current && !w.locked;
                resp.on_hover_text(tip).context_menu(|ui| {
                    if ui.add_enabled(!w.is_current && !w.prunable, egui::Button::new("Open")).clicked() {
                        actions.push(Action::OpenWorktree(wp.clone()));
                    }
                    if ui.button("Copy folder path").clicked() {
                        actions.push(Action::Copy(wp.display().to_string()));
                    }
                    ui.separator();
                    for force in [false, true] {
                        let (label, body) = if force {
                            ("Force remove…", format!("Delete the worktree folder {} even though it has uncommitted changes? They will be lost.", wp.display()))
                        } else {
                            ("Remove…", format!("Delete the worktree folder {}?\n\nIts branch and commits stay in the repository.", wp.display()))
                        };
                        if ui.add_enabled(removable, egui::Button::new(RichText::new(label).color(DANGER))).clicked() {
                            let target = wp.clone();
                            actions.push(Action::confirm(
                                "Remove worktree",
                                body,
                                "Remove",
                                true,
                                Action::GitThen {
                                    repo: path.clone(),
                                    label: format!("Remove worktree {}", git::repo_name(&target)),
                                    op: Box::new({
                                        let t = target.clone();
                                        move |r| git::remove_worktree(r, &t, force)
                                    }),
                                    then: FollowUp::Forget(target),
                                },
                            ));
                        }
                    }
                });
            }
            if shown.iter().any(|w| w.prunable) && side_row(ui, "⌫", WARN, "Clean up missing worktrees", None, false).clicked() {
                actions.push(Action::args(&path, "Clean up worktrees", &["worktree", "prune"]));
            }
            if side_row(ui, "+", ACCENT, "New worktree…", None, false).on_hover_text("Work on another branch in a second folder, side by side").clicked() {
                actions.push(new_worktree(&path, data, true, "", "HEAD", &format!("the current branch ({})", if current.is_empty() { "HEAD" } else { &current })));
            }
        });

        section(ui, "submodules", "SUBMODULES", data.submodules.len(), !data.submodules.is_empty(), |ui| {
            for sm in &data.submodules {
                let (note, nc) = match sm.state {
                    git::SubState::Ready => (sm.sha[..7.min(sm.sha.len())].to_string(), DIM),
                    git::SubState::NotSetUp => ("not set up".to_string(), WARN),
                    git::SubState::Moved => ("moved".to_string(), WARN),
                    git::SubState::Conflict => ("conflict".to_string(), DANGER),
                };
                let folder = path.join(&sm.path);
                let ready = sm.state != git::SubState::NotSetUp;
                let resp = side_row(ui, "◫", BLUE, &sm.path, Some((note, nc)), false);
                if resp.clicked() {
                    actions.push(if ready {
                        Action::OpenRepo(folder.clone())
                    } else {
                        let p = sm.path.clone();
                        Action::git(&path, format!("Set up submodule {p}"), move |r| git::update_submodules(r, Some(&p), false))
                    });
                }
                let mut tip = format!("{}\n{}", sm.path, sm.url.as_deref().unwrap_or("(no address)"));
                tip += match sm.state {
                    git::SubState::Ready => "\nUp to date with the recorded commit · click to open it",
                    git::SubState::NotSetUp => "\nNot downloaded yet · click to set it up",
                    git::SubState::Moved => "\nChecked out at a different commit than recorded · click to open it",
                    git::SubState::Conflict => "\nHas conflicts · click to open it",
                };
                let sm2 = sm.clone();
                resp.on_hover_text(tip).context_menu(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("Open")).clicked() {
                        actions.push(Action::OpenRepo(folder.clone()));
                    }
                    let p = sm2.path.clone();
                    let label = if ready { "Reset to recorded commit" } else { "Set up" };
                    if ui.button(label).on_hover_text("Download it if needed and check out the commit this repository records").clicked() {
                        let p = p.clone();
                        actions.push(Action::git(&path, format!("Update submodule {p}"), move |r| git::update_submodules(r, Some(&p), false)));
                    }
                    if ui.button("Update to its latest commit").on_hover_text("Fetch the submodule's own branch and move to its newest commit. Stage and commit to record it here.").clicked() {
                        let p = p.clone();
                        actions.push(Action::git(&path, format!("Update submodule {p} to latest"), move |r| git::update_submodules(r, Some(&p), true)));
                    }
                    ui.separator();
                    if ui.button("Copy folder path").clicked() {
                        actions.push(Action::Copy(folder.display().to_string()));
                    }
                    if let Some(u) = &sm2.url {
                        if ui.button("Copy address").clicked() {
                            actions.push(Action::Copy(u.clone()));
                        }
                    }
                    ui.separator();
                    if ui.button(RichText::new("Remove submodule…").color(DANGER)).clicked() {
                        let s3 = sm2.clone();
                        actions.push(Action::confirm(
                            "Remove submodule",
                            format!(
                                "Remove the submodule “{}” from this repository?\n\nIts folder and its downloaded copy are deleted, including any uncommitted work inside it. The removal is staged for you to commit.",
                                sm2.path
                            ),
                            "Remove",
                            true,
                            Action::git(&path, format!("Remove submodule {}", sm2.path), move |r| git::remove_submodule(r, &s3)),
                        ));
                    }
                });
            }
            let needs_update = data.submodules.iter().any(|s| matches!(s.state, git::SubState::NotSetUp | git::SubState::Moved));
            if needs_update
                && side_row(ui, "⟳", WARN, "Update all submodules", None, false)
                    .on_hover_text("Set up missing submodules and check out the recorded commit in each")
                    .clicked()
            {
                actions.push(Action::git(&path, "Update submodules", |r| git::update_submodules(r, None, false)));
            }
            if side_row(ui, "+", ACCENT, "Add submodule…", None, false).on_hover_text("Include another repository inside this one").clicked() {
                actions.push(Action::Dialog(Dialog::AddSubmodule { repo: path.clone(), url: String::new(), path: String::new(), path_edited: false }));
            }
        });

        let remotes: Vec<_> = data.branches.iter().filter(|b| b.remote).collect();
        section(ui, "remotes", "REMOTES", data.remotes.len(), true, |ui| {
            for r in &data.remotes {
                let url = data.remote_urls.iter().find(|(n, _)| n == r).map(|(_, u)| u.clone());
                let header = egui::CollapsingHeader::new(RichText::new(format!("☁  {r}")).size(13.0))
                    .id_salt(("remote", r))
                    .default_open(true)
                    .show(ui, |ui| {
                        for b in remotes.iter().filter(|b| b.name.starts_with(&format!("{r}/"))) {
                            let short = &b.name[r.len() + 1..];
                            let full_ref = format!("refs/remotes/{}", b.name);
                            let eye = Eye { hidden: data.hidden.contains(&full_ref), enabled: true };
                            let (resp, eye_clicked) = side_row_ex(ui, "⎇", DIM, short, None, false, Some(eye));
                            if eye_clicked {
                                actions.push(Action::ToggleHidden(full_ref));
                            }
                            let full = b.name.clone();
                            let local = short.to_string();
                            let exists = data.branches.iter().any(|x| !x.remote && x.name == local);
                            if resp.double_clicked() {
                                actions.push(checkout_remote(&path, &full, &local, exists));
                            } else if resp.clicked() {
                                actions.push(Action::SelectRef(full.clone()));
                            }
                            resp.on_hover_text(format!("{full}\nDouble-click to work on it locally")).context_menu(|ui| {
                                if ui.button(format!("Check out as “{local}”")).clicked() {
                                    actions.push(checkout_remote(&path, &full, &local, exists));
                                }
                                if !current.is_empty() && ui.button(format!("Merge {full} into {current}")).clicked() {
                                    actions.push(Action::args(&path, format!("Merge {full}"), &["merge", "--no-edit", &full]).undoable(git::Undoable::Tree { save_work: false }, format!("merge {full}")));
                                }
                                if ui.button("New branch from here…").clicked() {
                                    actions.push(new_branch(&path, &full, &full));
                                }
                            });
                        }
                    });
                let tip = match &url {
                    Some(u) => format!("{r}\n{u}\nRight-click for options"),
                    None => format!("{r}\nRight-click for options"),
                };
                header.header_response.on_hover_text(tip).context_menu(|ui| {
                    if ui.button(format!("Fetch {r}")).clicked() {
                        actions.push(Action::args(&path, format!("Fetch {r}"), &["fetch", "--prune", r]));
                    }
                    if let Some(u) = &url {
                        if ui.button("Copy address").clicked() {
                            actions.push(Action::Copy(u.clone()));
                        }
                    }
                    ui.separator();
                    if ui.button(RichText::new(format!("Remove {r}…")).color(DANGER)).clicked() {
                        let name = r.clone();
                        actions.push(Action::confirm(
                            "Remove remote",
                            format!(
                                "Stop tracking the remote “{r}”{}?\n\nIts branches disappear from this list. Nothing on the server or in your own branches is deleted, and you can add it again later.",
                                url.as_deref().map(|u| format!(" ({u})")).unwrap_or_default()
                            ),
                            "Remove",
                            true,
                            Action::git(&path, format!("Remove remote {r}"), move |p| git::remove_remote(p, &name)),
                        ));
                    }
                });
            }
            let add = side_row(ui, "+", ACCENT, "Add remote…", None, false);
            if add.on_hover_text("Connect this repository to a copy on a server, to fetch from and push to").clicked() {
                actions.push(Action::Dialog(Dialog::AddRemote {
                    repo: path.clone(),
                    name: if data.remotes.is_empty() { "origin".into() } else { String::new() },
                    url: String::new(),
                    fetch: true,
                    taken: data.remotes.clone(),
                }));
            }
        });

        section(ui, "tags", "TAGS", data.tags.len(), false, |ui| {
            for t in &data.tags {
                let resp = side_row(ui, "◆", WARN, t, None, false);
                if resp.clicked() {
                    actions.push(Action::SelectRef(t.clone()));
                }
                let t = t.clone();
                resp.context_menu(|ui| {
                    if ui.button("Check out this tag").clicked() {
                        actions.push(Action::confirm(
                            "Check out tag",
                            format!("Look at the files as they were at “{t}”? You won't be on a branch until you switch back."),
                            "Check out",
                            false,
                            checkout(&path, &t),
                        ));
                    }
                    if let Some(r) = data.remotes.first() {
                        if ui.button(format!("Push to {r}")).clicked() {
                            actions.push(Action::args(&path, format!("Push tag {t}"), &["push", r, &t]));
                        }
                    }
                    if ui.button("Copy name").clicked() {
                        actions.push(Action::Copy(t.clone()));
                    }
                    if ui.button(RichText::new("Delete…").color(DANGER)).clicked() {
                        actions.push(Action::confirm(
                            "Delete tag",
                            format!("Delete the tag “{t}” from this computer?"),
                            "Delete",
                            true,
                            Action::args(&path, format!("Delete tag {t}"), &["tag", "-d", &t]).undoable(git::Undoable::Refs, format!("delete tag {t}")),
                        ));
                    }
                });
            }
        });

        section(ui, "stashes", "STASHES", data.stashes.len(), true, |ui| {
            if data.stashes.is_empty() {
                ui.label(RichText::new("   Nothing stashed").small().color(FAINT));
            }
            for s in &data.stashes {
                let r = format!("stash@{{{}}}", s.index);
                let resp = side_row(ui, "⤓", BLUE, &s.message, None, false);
                resp.on_hover_text(format!("{}\nRight-click for options", s.message)).context_menu(|ui| {
                    if ui.button("Apply (keep stash)").clicked() {
                        actions.push(Action::args(&path, "Apply stash", &["stash", "apply", &r]));
                    }
                    if ui.button("Pop (apply and remove)").clicked() {
                        actions.push(Action::args(&path, "Restore stash", &["stash", "pop", &r]));
                    }
                    if ui.button(RichText::new("Drop…").color(DANGER)).clicked() {
                        actions.push(Action::confirm(
                            "Drop stash",
                            format!("Throw away the stashed changes “{}”?", s.message),
                            "Drop",
                            true,
                            Action::args(&path, "Drop stash", &["stash", "drop", &r]).undoable(git::Undoable::Refs, format!("drop stash “{}”", s.message)),
                        ));
                    }
                });
            }
        });
    });
}

fn checkout_remote(path: &Path, full: &str, local: &str, exists: bool) -> Action {
    if exists {
        checkout(path, local)
    } else {
        Action::args(path, format!("Check out {full}"), &["checkout", "--track", full]).undoable(git::Undoable::Tree { save_work: false }, format!("check out {full}"))
    }
}

// ---------------------------------------------------------------------------
// Commit list with graph

const ROW_H: f32 = 26.0;
const LANE_W: f32 = 14.0;

fn commit_list(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    rv.refilter();
    let Some(data) = &rv.data else { return };
    let path = rv.path.clone();
    let searching = !rv.search.trim().is_empty();
    let more = data.commits.len() >= data.limit;
    let n_rows = rv.filtered.len() + 1 + more as usize;
    let graph_lanes = if searching { 1 } else { data.graph.width.clamp(1, 14) };
    let graph_w = graph_lanes as f32 * LANE_W + 10.0;
    let changes = data.status.change_count();
    // Columns shrink on narrow windows so the commit messages keep their room.
    let list_w = ui.available_width();
    let author_w: f32 = if list_w < 520.0 { 0.0 } else if list_w < 700.0 { 110.0 } else { 150.0 };
    let date_w: f32 = if list_w < 420.0 { 0.0 } else { 100.0 };
    // The commit id is the last column to disappear on narrow windows.
    let hash_w: f32 = if list_w < 280.0 { 0.0 } else { 74.0 };
    // Space taken by the right-hand columns (author, commit id, date).
    let cols_w = author_w + hash_w + date_w;

    // Keyboard navigation through the list.
    let cur_row = match &rv.selection {
        Some(Selection::WorkingDir) | None => Some(0),
        Some(Selection::Commit(h)) => rv.filtered.iter().position(|&i| data.commits[i].hash == *h).map(|p| p + 1),
        Some(Selection::Reflog) => None,
    };
    let mut scroll_offset = None;
    // Not while a dialog is open: its keys are its own.
    if !rv.modal && !ui.ctx().text_edit_focused() && !ui.ctx().any_popup_open() {
        let (offset, view_h) = rv.list_view;
        let page = ((view_h / ROW_H).floor() as usize).saturating_sub(1).max(1);
        let key = ui.input(|i| {
            [
                (egui::Key::ArrowUp, NavKey::Up),
                (egui::Key::ArrowDown, NavKey::Down),
                (egui::Key::PageUp, NavKey::PageUp),
                (egui::Key::PageDown, NavKey::PageDown),
                (egui::Key::Home, NavKey::Home),
                (egui::Key::End, NavKey::End),
            ]
            .into_iter()
            .find(|(k, _)| i.key_pressed(*k))
            .map(|(_, n)| n)
        });
        // Row 0 is "uncommitted changes"; commits follow.
        if let (Some(cur), Some(key)) = (cur_row, key) {
            if let Some(t) = nav_target(cur, rv.filtered.len() + 1, key, page) {
                if t == 0 {
                    actions.push(Action::SelectWorkingDir);
                } else {
                    actions.push(Action::SelectCommit(data.commits[rv.filtered[t - 1]].hash.clone()));
                }
                // Keep the newly selected row on screen; End shows the very bottom
                // (including "Load more history").
                scroll_offset = if key == NavKey::End { Some(f32::MAX) } else { keep_visible(t, offset, view_h, ROW_H) };
            }
        }
    }

    // Column header
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    {
        let p = ui.painter();
        p.rect_filled(hr, 0.0, PANEL);
        p.line_segment([hr.left_bottom(), hr.right_bottom()], Stroke::new(1.0, BORDER));
        let f = theme::prop(11.5);
        let y = hr.center().y;
        let title = if searching { format!("{} MATCHING COMMITS", rv.filtered.len()) } else { format!("HISTORY · {} COMMITS", data.commits.len()) };
        p.text(pos2(hr.left() + 10.0, y), Align2::LEFT_CENTER, title, f.clone(), DIM);
        if author_w > 0.0 {
            p.text(pos2(hr.right() - cols_w, y), Align2::LEFT_CENTER, "AUTHOR", f.clone(), DIM);
        }
        if hash_w > 0.0 {
            p.text(pos2(hr.right() - date_w - hash_w, y), Align2::LEFT_CENTER, "COMMIT", f.clone(), DIM);
        }
        if date_w > 0.0 {
            p.text(pos2(hr.right() - date_w, y), Align2::LEFT_CENTER, "DATE", f, DIM);
        }
    }

    let mut sa = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("commits", &path));
    if let Some(i) = rv.list_scroll_to.take() {
        let pos = rv.filtered.iter().position(|&x| x == i).unwrap_or(0) + 1;
        sa = sa.vertical_scroll_offset(((pos as f32 - 4.0) * ROW_H).clamp(0.0, furthest_list_offset(ui, n_rows)));
    } else if let Some(off) = scroll_offset {
        sa = sa.vertical_scroll_offset(off.min(furthest_list_offset(ui, n_rows)));
    }
    let selected_hash = match &rv.selection {
        Some(Selection::Commit(h)) => Some(h.clone()),
        _ => None,
    };
    let head_oid = data.status.head.oid.clone();
    let head_row_col = data.commits.first().filter(|c| Some(&c.hash) == head_oid.as_ref()).map(|_| data.graph.rows[0].col);

    let mut rv_list_view = rv.list_view;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let out = sa.show_rows(ui, ROW_H, n_rows, |ui, range| {
            for r in range {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let p = ui.painter_at(rect);
                let selected = if r == 0 { selected_hash.is_none() } else { false };
                if r == 0 {
                    // Working directory row
                    paint_row_bg(&p, rect, selected, resp.hovered());
                    let col = head_row_col.unwrap_or(0);
                    let cx = rect.left() + 8.0 + col as f32 * LANE_W + LANE_W / 2.0;
                    if head_row_col.is_some() && !searching {
                        dashed(&p, pos2(cx, rect.center().y), pos2(cx, rect.bottom()), theme::lane(col));
                    }
                    let c = if changes > 0 { WARN } else { FAINT };
                    p.circle_filled(pos2(cx, rect.center().y), 5.0, BG);
                    p.circle_stroke(pos2(cx, rect.center().y), 4.5, Stroke::new(2.0, c));
                    let text = if changes > 0 {
                        format!("Uncommitted changes  ·  {changes} file{}", if changes == 1 { "" } else { "s" })
                    } else {
                        "Working directory is clean".to_string()
                    };
                    name_it(&resp, egui::Role::Row, if changes > 0 { "Uncommitted changes" } else { "Working directory is clean" });
                    p.text(pos2(rect.left() + graph_w + 6.0, rect.center().y), Align2::LEFT_CENTER, text, theme::prop(13.5), if changes > 0 { TEXT } else { DIM });
                    if resp.clicked() {
                        actions.push(Action::SelectWorkingDir);
                    }
                    continue;
                }
                if r == rv.filtered.len() + 1 {
                    let b = ui.put(rect.shrink2(vec2(rect.width() / 2.0 - 110.0, 2.0)), egui::Button::new("Load more history"));
                    if b.clicked() {
                        actions.push(Action::LoadMore);
                    }
                    continue;
                }
                let ci = rv.filtered[r - 1];
                let c = &data.commits[ci];
                name_it(&resp, egui::Role::Row, &c.summary);
                let selected = selected_hash.as_deref() == Some(c.hash.as_str());
                paint_row_bg(&p, rect, selected, resp.hovered());

                // Graph
                let gp = p.with_clip_rect(Rect::from_min_max(rect.min, pos2(rect.left() + graph_w, rect.bottom())));
                let x = |i: usize| rect.left() + 8.0 + i as f32 * LANE_W + LANE_W / 2.0;
                let (top, mid, bot) = (rect.top(), rect.center().y, rect.bottom());
                let dot_col = if searching { 0 } else { data.graph.rows[ci].col };
                if !searching {
                    let row = &data.graph.rows[ci];
                    for (i, t) in &row.top {
                        match t {
                            Target::Through => {
                                gp.line_segment([pos2(x(*i), top), pos2(x(*i), bot)], Stroke::new(2.0, theme::lane(*i)));
                            }
                            Target::Dot => curve(&gp, pos2(x(*i), top), pos2(x(row.col), mid), theme::lane(*i)),
                        }
                    }
                    for &t in &row.to_parents {
                        curve(&gp, pos2(x(row.col), mid), pos2(x(t), bot), theme::lane(t));
                    }
                    if ci == 0 && head_row_col.is_some() {
                        dashed(&gp, pos2(x(row.col), top), pos2(x(row.col), mid), theme::lane(row.col));
                    }
                }
                let dc = theme::lane(dot_col);
                let center = pos2(x(dot_col), mid);
                if c.parents.len() > 1 {
                    gp.circle_filled(center, 5.0, BG);
                    gp.circle_stroke(center, 4.0, Stroke::new(2.5, dc));
                } else {
                    gp.circle_filled(center, 5.5, BG);
                    gp.circle_filled(center, 4.5, dc);
                }

                // Labels and summary
                let mut tx = rect.left() + graph_w + 6.0;
                for rl in &c.refs {
                    let (fill, fg, icon) = match rl.kind {
                        RefKind::Head => (ACCENT_DIM, ACCENT, "✔ "),
                        RefKind::Local => (Color32::from_rgb(34, 54, 82), BLUE, ""),
                        RefKind::Remote => (Color32::from_rgb(50, 40, 70), PURPLE, "☁ "),
                        RefKind::Tag => (Color32::from_rgb(70, 56, 30), WARN, "◆ "),
                    };
                    let g = p.layout_no_wrap(format!("{icon}{}", rl.name), theme::prop(11.5), fg);
                    let w = g.size().x + 12.0;
                    if tx + w > rect.right() - cols_w - 80.0 {
                        break;
                    }
                    let br = Rect::from_min_size(pos2(tx, mid - 9.0), vec2(w, 18.0));
                    p.rect_filled(br, 9.0, fill);
                    p.galley(pos2(tx + 6.0, mid - g.size().y / 2.0), g, fg);
                    tx += w + 5.0;
                }
                let max_w = (rect.right() - cols_w - tx - 12.0).max(20.0);
                let g = trunc(&p, &c.summary, theme::prop(13.5), TEXT, max_w);
                p.galley(pos2(tx, mid - g.size().y / 2.0), g, TEXT);
                if author_w > 0.0 {
                    let ga = trunc(&p, &c.author, theme::prop(12.5), DIM, author_w - 12.0);
                    p.galley(pos2(rect.right() - cols_w, mid - ga.size().y / 2.0), ga, DIM);
                }
                if hash_w > 0.0 {
                    let hc = if selected { TEXT } else { FAINT };
                    p.text(pos2(rect.right() - date_w - hash_w, mid), Align2::LEFT_CENTER, c.short(), theme::mono(12.0), hc);
                }
                if date_w > 0.0 {
                    let gd = trunc(&p, &theme::relative(c.time), theme::prop(12.5), DIM, date_w - 8.0);
                    p.galley(pos2(rect.right() - date_w, mid - gd.size().y / 2.0), gd, DIM);
                }

                if resp.clicked() {
                    actions.push(Action::SelectCommit(c.hash.clone()));
                }
                let current = data.status.head.branch.clone();
                resp.on_hover_text_at_pointer(format!("{}\n{} <{}> · {}\n{}", c.summary, c.author, c.email, theme::date_time(c.time), c.short()))
                    .context_menu(|ui| commit_menu(ui, &path, data, c, current.as_deref(), actions));
            }
        });
        rv_list_view = (out.state.clamped_offset().y, out.inner_rect.height());
    });
    rv.list_view = rv_list_view;
}

/// How far the history list can scroll (so a jump never overshoots and slides back).
fn furthest_list_offset(ui: &Ui, rows: usize) -> f32 {
    (rows as f32 * ROW_H - ui.available_height()).max(0.0)
}

/// A key that moves the selection in the history list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NavKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The row a key moves to from `cur`, in a list of `len` rows; `None` if it doesn't move.
pub fn nav_target(cur: usize, len: usize, key: NavKey, page: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len - 1;
    let t = match key {
        NavKey::Up => cur.saturating_sub(1),
        NavKey::Down => (cur + 1).min(last),
        NavKey::PageUp => cur.saturating_sub(page),
        NavKey::PageDown => (cur + page).min(last),
        NavKey::Home => 0,
        NavKey::End => last,
    };
    (t != cur).then_some(t)
}

/// The scroll position that brings `row` into view, or `None` if it's already visible.
pub fn keep_visible(row: usize, offset: f32, view_h: f32, row_h: f32) -> Option<f32> {
    let top = row as f32 * row_h;
    let bottom = top + row_h;
    if top < offset {
        Some(top)
    } else if view_h > 0.0 && bottom > offset + view_h {
        Some((bottom - view_h).max(0.0))
    } else {
        None
    }
}

fn commit_menu(ui: &mut Ui, path: &Path, data: &RepoData, c: &git::Commit, current: Option<&str>, actions: &mut Vec<Action>) {
    let short = c.short().to_string();
    let hash = c.hash.clone();
    let cur = current.unwrap_or("the current position").to_string();
    if ui.button("New branch here…").clicked() {
        actions.push(new_branch(path, &hash, &format!("commit {short}")));
    }
    if ui.button("New worktree here…").on_hover_text("A second folder with a new branch starting at this commit").clicked() {
        actions.push(new_worktree(path, data, true, "", &hash, &format!("commit {short}")));
    }
    let editable = data.on_branch.contains(&hash);
    let tip = if editable { "Change this commit's message" } else { "Only commits on your branches can be edited (this one is only on a remote)" };
    if ui.add_enabled(editable, egui::Button::new("Edit commit message…")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
        actions.push(Action::EditMessage(hash.clone()));
    }
    ui.separator();
    if ui.button("New tag here…").clicked() {
        actions.push(Action::Dialog(Dialog::CreateTag {
            repo: path.to_path_buf(),
            at: hash.clone(),
            label: format!("commit {short}"),
            name: String::new(),
        }));
    }
    if ui.button("Check out this commit…").clicked() {
        actions.push(Action::confirm(
            "Check out commit",
            format!("Look at the files exactly as they were at {short}? You won't be on a branch until you switch back."),
            "Check out",
            false,
            Action::args(path, format!("Check out {short}"), &["checkout", &hash]).undoable(git::Undoable::Tree { save_work: false }, format!("check out {short}")),
        ));
    }
    ui.separator();
    if ui.button(format!("Copy this commit onto {cur} (cherry-pick)")).clicked() {
        actions.push(Action::confirm(
            "Cherry-pick",
            format!("Apply the changes of “{}” on top of {cur}?", c.summary),
            "Cherry-pick",
            false,
            Action::args(path, format!("Cherry-pick {short}"), &["cherry-pick", &hash]).undoable(git::Undoable::Tree { save_work: false }, format!("cherry-pick {short}")),
        ));
    }
    if ui.button("Undo this commit with a new commit (revert)").clicked() {
        actions.push(Action::confirm(
            "Revert commit",
            format!("Create a new commit that undoes “{}”?", c.summary),
            "Revert",
            false,
            Action::args(path, format!("Revert {short}"), &["revert", "--no-edit", &hash]).undoable(git::Undoable::Tree { save_work: false }, format!("revert {short}")),
        ));
    }
    reset_menu(ui, path, data, &hash, actions);
    ui.separator();
    if ui.button("Copy commit id").clicked() {
        actions.push(Action::Copy(hash.clone()));
    }
    if ui.button("Copy message").clicked() {
        actions.push(Action::Copy(c.summary.clone()));
    }
}

/// "Move <branch> to this commit", with the three kinds of reset. The confirmation
/// names what moves, from where to where.
fn reset_menu(ui: &mut Ui, path: &Path, data: &RepoData, hash: &str, actions: &mut Vec<Action>) {
    let head = &data.status.head;
    let short = &hash[..hash.len().min(7)];
    let cur = head.branch.clone().unwrap_or_else(|| "the current position".into());
    let moves = match (&head.branch, head.oid.as_deref()) {
        (_, Some(o)) if o == hash => format!("{} is already at {short}.", head.branch.as_deref().unwrap_or("HEAD")),
        (b, Some(o)) => format!("{} moves from {} to {short}.", b.as_deref().unwrap_or("HEAD (detached)"), &o[..o.len().min(7)]),
        (b, None) => format!("{} moves to {short}.", b.as_deref().unwrap_or("HEAD")),
    };
    ui.menu_button(format!("Move {cur} to this commit"), |ui| {
        for (mode, label, desc, danger) in [
            ("--soft", "Keep all changes, staged", "Your later commits' changes stay staged, ready to commit again.", false),
            ("--mixed", "Keep all changes, unstaged", "Your later commits' changes stay in your files, unstaged.", false),
            ("--hard", "Discard everything after it", "Later commits and ALL uncommitted changes are thrown away.", true),
        ] {
            let text = if danger { RichText::new(label).color(DANGER) } else { RichText::new(label) };
            if ui.button(text).on_hover_text(desc).clicked() {
                actions.push(Action::confirm(
                    "Reset branch",
                    format!("Move {cur} to {short}?\n\n{moves}\n{desc}\n\nYou can undo this afterwards (Ctrl+Z)."),
                    "Reset",
                    danger,
                    Action::args(path, format!("Reset to {short}"), &["reset", mode, hash]).undoable(reset_undo(mode), format!("reset {cur} to {short}")),
                ));
            }
        }
    });
}

/// What a `git reset` in this mode changes.
fn reset_undo(mode: &str) -> git::Undoable {
    if mode == "--hard" { git::Undoable::Tree { save_work: true } } else { git::Undoable::Index }
}

pub(crate) fn paint_row_bg(p: &egui::Painter, rect: Rect, selected: bool, hovered: bool) {
    if selected {
        p.rect_filled(rect, 0.0, SELECT);
        p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, ACCENT);
    } else if hovered {
        p.rect_filled(rect, 0.0, HOVER);
    }
}

fn curve(p: &egui::Painter, a: Pos2, b: Pos2, color: Color32) {
    if (a.x - b.x).abs() < 0.5 {
        p.line_segment([a, b], Stroke::new(2.0, color));
        return;
    }
    let pts: Vec<Pos2> = (0..=10)
        .map(|i| {
            let t = i as f32 / 10.0;
            let s = t * t * (3.0 - 2.0 * t);
            pos2(a.x + (b.x - a.x) * s, a.y + (b.y - a.y) * t)
        })
        .collect();
    p.line(pts, Stroke::new(2.0, color));
}

fn dashed(p: &egui::Painter, a: Pos2, b: Pos2, color: Color32) {
    let len = (b.y - a.y).abs();
    let mut y = 0.0;
    while y < len {
        let y2 = (y + 3.0).min(len);
        p.line_segment([pos2(a.x, a.y + y), pos2(a.x, a.y + y2)], Stroke::new(1.5, color.gamma_multiply(0.7)));
        y += 6.0;
    }
}

pub fn trunc(p: &egui::Painter, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat::simple(font, color));
    job.wrap = TextWrapping { max_width: max_w.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    p.layout_job(job)
}

// ---------------------------------------------------------------------------
// Reflog

fn reflog_panel(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let path = rv.path.clone();
    let Some(data) = &rv.data else { return };
    let shown = rv.reflog_ref.clone();
    egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("REFLOG").size(11.5).strong().color(DIM));
            ui.add_space(6.0);
            let picker = egui::ComboBox::from_id_salt(("reflog_ref", &path))
                .selected_text(shown.strip_prefix("refs/heads/").unwrap_or(&shown))
                .width(180.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(shown == "HEAD", "HEAD").clicked() {
                        actions.push(Action::ShowReflog(Some("HEAD".into())));
                    }
                    for b in data.branches.iter().filter(|b| !b.remote) {
                        let full = format!("refs/heads/{}", b.name);
                        if ui.selectable_label(shown == full, &b.name).clicked() {
                            actions.push(Action::ShowReflog(Some(full)));
                        }
                    }
                });
            picker.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, format!("Reflog of {shown}")));
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Everywhere it has pointed, newest first. Right-click an entry to get back to it.").small().color(FAINT));
    });
    let list = match &rv.reflog {
        None => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(20.0).color(ACCENT)));
            return;
        }
        Some(Err(e)) => {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(format!("Couldn't read the reflog\n{e}")).color(WARN)));
            return;
        }
        Some(Ok(list)) if list.is_empty() => {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Nothing recorded yet").color(FAINT)));
            return;
        }
        Some(Ok(list)) => list,
    };
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_H, list.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for e in &list[range] {
            reflog_row(ui, &path, data, e, actions);
        }
    });
}

fn reflog_color(action: &str) -> Color32 {
    match action.split([' ', ':']).next().unwrap_or("") {
        "commit" => ACCENT,
        "reset" => WARN,
        "checkout" => BLUE,
        "merge" | "pull" | "rebase" | "cherry-pick" | "revert" => PURPLE,
        _ => DIM,
    }
}

fn reflog_row(ui: &mut Ui, path: &Path, data: &RepoData, e: &git::ReflogEntry, actions: &mut Vec<Action>) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    let short = &e.hash[..e.hash.len().min(7)];
    name_it(&resp, egui::Role::Button, format!("{short} {}: {}", e.action, e.message));
    let p = ui.painter();
    paint_row_bg(p, rect, false, resp.hovered());
    let y = rect.center().y;
    let mut x = rect.left() + 16.0;
    p.text(pos2(x, y), Align2::LEFT_CENTER, short, theme::mono(12.0), DIM);
    x += 64.0;
    let action = trunc(p, &e.action, theme::prop(12.0), reflog_color(&e.action), 110.0);
    let aw = action.size().x;
    p.galley(pos2(x, y - action.size().y / 2.0), action, TEXT);
    x += aw.max(70.0) + 10.0;
    let when = p.layout_no_wrap(theme::relative(e.time), theme::prop(12.0), FAINT);
    let when_w = when.size().x;
    p.galley(pos2(rect.right() - 16.0 - when_w, y - when.size().y / 2.0), when, FAINT);
    let msg_w = (rect.right() - 28.0 - when_w - x).max(0.0);
    let msg = trunc(p, &e.message, theme::prop(13.0), TEXT, msg_w);
    p.galley(pos2(x, y - msg.size().y / 2.0), msg, TEXT);

    let in_history = data.commits.iter().any(|c| c.hash == e.hash);
    if resp.double_clicked() && in_history {
        actions.push(Action::SelectCommit(e.hash.clone()));
    }
    let tip = if in_history { "Double-click to show it in the history · right-click for options" } else { "Not on any branch any more · right-click for options" };
    resp.on_hover_text(format!("{}\n{}\n{tip}", e.hash, theme::date_time(e.time))).context_menu(|ui| {
        if ui.button("New branch here…").clicked() {
            actions.push(new_branch(path, &e.hash, &format!("commit {short}")));
        }
        if ui.button("Check out (detached)…").clicked() {
            actions.push(Action::confirm(
                "Check out commit",
                format!("Look at the files exactly as they were at {short}? You won't be on a branch until you switch back."),
                "Check out",
                false,
                Action::args(path, format!("Check out {short}"), &["checkout", "--detach", &e.hash])
                    .undoable(git::Undoable::Tree { save_work: false }, format!("check out {short}")),
            ));
        }
        reset_menu(ui, path, data, &e.hash, actions);
        ui.separator();
        if ui.button("Copy commit id").clicked() {
            actions.push(Action::Copy(e.hash.clone()));
        }
    });
}

// ---------------------------------------------------------------------------
// Commit details

fn commit_panel(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let Some(d) = &rv.detail else {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.add(egui::Spinner::new().size(20.0).color(ACCENT));
        });
        return;
    };
    let info = &d.info;
    let mut jump = None;
    let mut fold_all = None;
    // Each commit starts with its message, file list and diff scrolled to the top.
    let reset = std::mem::take(&mut rv.reset_detail_scroll);
    if reset {
        rv.diff_scroll_to = Some(0);
    }
    egui::Panel::top("commit_meta")
        .resizable(true)
        .default_size(240.0)
        .size_range(120.0..=600.0)
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(16, 12)))
        .show(ui, |ui| {
            let mut meta_scroll = egui::ScrollArea::vertical().auto_shrink(false);
            if reset {
                meta_scroll = meta_scroll.vertical_scroll_offset(0.0);
            }
            meta_scroll.show(ui, |ui| {
                let mut lines = info.message.splitn(2, '\n');
                let title = lines.next().unwrap_or("");
                let body = lines.next().unwrap_or("").trim();
                let editable = rv.data.as_ref().is_some_and(|data| data.on_branch.contains(&info.hash));
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let tip = if editable {
                            "Change this commit's message"
                        } else {
                            "Only commits on your branches can be edited (this one is only on a remote)."
                        };
                        if ui.add_enabled(editable, egui::Button::new("✎ Edit message").small()).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                            actions.push(Action::EditMessage(info.hash.clone()));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                            ui.add(egui::Label::new(RichText::new(title).size(16.0).strong()).wrap());
                        });
                    });
                });
                if !body.is_empty() {
                    ui.label(RichText::new(body).color(DIM));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 16.0, theme::lane(info.author.len()).gamma_multiply(0.3));
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, theme::initials(&info.author), theme::prop(12.5), TEXT);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(format!("{}  <{}>", info.author, info.email)).color(TEXT));
                        let mut when = format!("{}  ·  {}", theme::relative(info.time), theme::date_time(info.time));
                        if info.committer != info.author {
                            when += &format!("  ·  committed by {}", info.committer);
                        }
                        ui.label(RichText::new(when).small().color(DIM));
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Commit").small().color(DIM));
                    ui.label(RichText::new(&info.hash[..12.min(info.hash.len())]).monospace());
                    if ui.small_button("Copy").clicked() {
                        actions.push(Action::Copy(info.hash.clone()));
                    }
                    if !info.parents.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new(if info.parents.len() > 1 { "Parents" } else { "Parent" }).small().color(DIM));
                        for p in &info.parents {
                            if ui.link(RichText::new(&p[..8.min(p.len())]).monospace()).clicked() {
                                actions.push(Action::SelectCommit(p.clone()));
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                let (a, r): (u32, u32) = d.doc.files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} file{} changed", d.doc.files.len(), if d.doc.files.len() == 1 { "" } else { "s" })).strong());
                    ui.label(RichText::new(format!("+{a}")).color(ADD_FG));
                    ui.label(RichText::new(format!("−{r}")).color(DEL_FG));
                    if !d.doc.files.is_empty() {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some(all) = fold_buttons(ui, &d.doc) {
                                fold_all = Some(all);
                            }
                        });
                    }
                });
                ui.spacing_mut().item_spacing.y = 1.0;
                for (i, f) in d.doc.files.iter().enumerate() {
                    let resp = file_line(ui, &f.path, f.old_path.as_deref(), &f.change, false, Some((f.added, f.removed)));
                    if resp.clicked() {
                        jump = Some(i);
                    }
                    resp.on_hover_text("Click to go to its changes · right-click for blame and history")
                        .context_menu(|ui| file_history_menu(ui, &f.path, Some(&info.hash), &f.change, actions));
                }
            });
        });
    let d = rv.detail.as_mut().unwrap();
    if let Some(all) = fold_all {
        d.doc.set_all_collapsed(all);
    }
    if let Some(i) = jump {
        // Picking a file in the list unfolds it and scrolls to it.
        d.doc.set_collapsed(i, false);
        rv.diff_scroll_to = d.doc.file_rows.get(i).copied();
    }
    let d = rv.detail.as_ref().unwrap();
    let toggled = egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| {
            commit_line_bar(ui, &d.doc, &mut rv.detail_sel, &d.info.hash, actions);
            diff_view(ui, &d.doc, DiffMode::Commit, &mut rv.diff_scroll_to, Some(&mut rv.detail_sel), &rv.path, actions)
        })
        .inner;
    if let Some(fi) = toggled {
        if let Some(d) = rv.detail.as_mut() {
            d.doc.toggle(fi);
        }
    }
}

fn change_color(c: &Change) -> Color32 {
    match c {
        Change::Modified | Change::TypeChange => WARN,
        Change::Added | Change::Untracked => ACCENT,
        Change::Deleted | Change::Conflict(_) => DANGER,
        Change::Renamed | Change::Copied => BLUE,
    }
}

/// One file in a list. Returns the row response.
pub(crate) fn file_line(ui: &mut Ui, path: &str, old: Option<&str>, change: &Change, selected: bool, stats: Option<(u32, u32)>) -> egui::Response {
    file_line_tagged(ui, path, old, change, selected, stats, None)
}

/// A file row with an optional small tag after the name (e.g. "submodule").
fn file_line_tagged(
    ui: &mut Ui,
    path: &str,
    old: Option<&str>,
    change: &Change,
    selected: bool,
    stats: Option<(u32, u32)>,
    tag: Option<&str>,
) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    name_it(&resp, egui::Role::Row, path);
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 4.0, SELECT);
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, HOVER);
    }
    let y = rect.center().y;
    let cc = change_color(change);
    let badge = Rect::from_center_size(pos2(rect.left() + 14.0, y), vec2(18.0, 16.0));
    p.rect_filled(badge, 3.0, cc.gamma_multiply(0.2));
    p.text(badge.center(), Align2::CENTER_CENTER, if *change == Change::Untracked { "+" } else { change.letter() }, theme::prop(11.0), cc);
    let mut right = 0.0;
    if let Some((a, r)) = stats {
        let t = format!("+{a} −{r}");
        let g = p.layout_no_wrap(t, theme::prop(11.5), DIM);
        right = g.size().x + 10.0;
        p.galley(pos2(rect.right() - g.size().x - 6.0, y - g.size().y / 2.0), g, DIM);
    }
    // File name bright, folder dim.
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let mut job = LayoutJob::default();
    job.append(name, 0.0, TextFormat::simple(theme::prop(13.0), TEXT));
    if let Some(t) = tag {
        job.append(&format!("  ◫ {t}"), 0.0, TextFormat::simple(theme::prop(11.5), BLUE));
    }
    let mut extra = String::new();
    if !dir.is_empty() {
        extra = format!("  {dir}");
    }
    if let Some(o) = old {
        extra += &format!("  (from {o})");
    }
    job.append(&extra, 0.0, TextFormat::simple(theme::prop(12.0), FAINT));
    job.wrap = TextWrapping { max_width: (rect.width() - 34.0 - right).max(20.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = p.layout_job(job);
    p.galley(pos2(rect.left() + 30.0, y - g.size().y / 2.0), g, TEXT);
    resp
}

// ---------------------------------------------------------------------------
// Working directory: staging and committing

fn staging_panel(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let Some(data) = &rv.data else { return };
    let st = &data.status;
    let path = rv.path.clone();

    // The file lists take a share of the height so the stacked diffs below always get room.
    let pane_h = ui.available_height();
    egui::Panel::top("stage_lists")
        .resizable(true)
        .default_size((pane_h * 0.28).clamp(90.0, 250.0))
        .size_range(80.0..=(pane_h * 0.45).max(90.0))
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(10, 8)))
        .show(ui, |ui| {
            // Only the visible rows are drawn, so thousands of changed files stay smooth.
            let mut sections: Vec<(ListKind, &[FileChange])> = Vec::new();
            if !st.conflicts.is_empty() {
                sections.push((ListKind::Conflicts, &st.conflicts));
            }
            sections.push((ListKind::Unstaged, &st.unstaged));
            sections.push((ListKind::Staged, &st.staged));
            // Each section: header, its files (or one "empty" line), then a spacer.
            let section_len = |files: &[FileChange]| files.len().max(1) + 2;
            let total: usize = sections.iter().map(|(_, f)| section_len(f)).sum();
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical().auto_shrink(false).id_salt(("stage_list", &path)).show_rows(ui, 24.0, total, |ui, range| {
                    for r in range {
                        let mut r = r;
                        let Some(&(kind, files)) = sections.iter().find(|(_, f)| {
                            let n = section_len(f);
                            if r < n { true } else { r -= n; false }
                        }) else { continue };
                        if r == 0 {
                            let (title, color, button) = match kind {
                                ListKind::Conflicts => ("CONFLICTS", DANGER, None),
                                ListKind::Unstaged => ("UNSTAGED", WARN, (!files.is_empty()).then_some("Stage all")),
                                ListKind::Staged => ("STAGED", ACCENT, (!files.is_empty()).then_some("Unstage all")),
                            };
                            if header_row(ui, title, files.len(), color, button) {
                                match kind {
                                    ListKind::Unstaged => actions.push(stage_all(&path)),
                                    ListKind::Staged => {
                                        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                                        actions.push(quiet(Action::git(&path, "Unstage all", move |p| git::unstage(p, &paths))));
                                    }
                                    ListKind::Conflicts => {}
                                }
                            }
                            continue;
                        }
                        let i = r - 1;
                        if i >= files.len().max(1) {
                            ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                            continue;
                        }
                        if files.is_empty() {
                            let text = match kind {
                                ListKind::Staged => "    Stage files to include them in the next commit",
                                _ => "    No unstaged changes",
                            };
                            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                            ui.painter().text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, text, theme::prop(11.5), FAINT);
                            continue;
                        }
                        let f = &files[i];
                        match kind {
                            ListKind::Conflicts => {
                                let (resp, b) = file_row_buttons(ui, f, false, &["Mark resolved"]);
                                resp.context_menu(|ui| file_history_menu(ui, &f.path, None, &f.change, actions));
                                if resp.clicked() {
                                    actions.push(Action::OpenConflict(f.path.clone()));
                                }
                                if b == Some(0) {
                                    actions.push(Action::args(&path, format!("Mark {} resolved", f.path), &["add", "--", &f.path]));
                                }
                            }
                            ListKind::Unstaged => {
                                let sel = FileSel { path: f.path.clone(), staged: false, untracked: f.change == Change::Untracked };
                                let second = if f.change == Change::Untracked { "Delete" } else { "Discard" };
                                let (resp, b) = file_row_buttons(ui, f, rv.file_sel.as_ref() == Some(&sel), &["Stage", second]);
                                resp.context_menu(|ui| file_history_menu(ui, &f.path, None, &f.change, actions));
                                if resp.clicked() {
                                    actions.push(Action::SelectFile(sel));
                                }
                                if resp.double_clicked() || b == Some(0) {
                                    let paths = vec![f.path.clone()];
                                    actions.push(quiet(Action::git(&path, format!("Stage {}", f.path), move |p| git::stage(p, &paths))));
                                }
                                if b == Some(1) {
                                    let fc = f.clone();
                                    let what = if f.submodule {
                            "put this submodule back on the commit recorded here"
                        } else if f.change == Change::Untracked {
                            "delete this new file"
                        } else {
                            "throw away your changes to this file"
                        };
                                    actions.push(Action::confirm(
                                        "Discard changes",
                                        format!("Really {what}?\n\n{}\n\n{}", f.path, discard_note(f)),
                                        "Discard",
                                        true,
                                        discard_file(&path, fc),
                                    ));
                                }
                            }
                            ListKind::Staged => {
                                let sel = FileSel { path: f.path.clone(), staged: true, untracked: false };
                                let (resp, b) = file_row_buttons(ui, f, rv.file_sel.as_ref() == Some(&sel), &["Unstage"]);
                                resp.context_menu(|ui| file_history_menu(ui, &f.path, None, &f.change, actions));
                                if resp.clicked() {
                                    actions.push(Action::SelectFile(sel));
                                }
                                if resp.double_clicked() || b == Some(0) {
                                    let mut paths = vec![f.path.clone()];
                                    if let Some(o) = &f.old_path {
                                        paths.push(o.clone());
                                    }
                                    actions.push(quiet(Action::git(&path, format!("Unstage {}", f.path), move |p| git::unstage(p, &paths))));
                                }
                            }
                        }
                    }
                });
            });
        });

    // Commit box
    let staged_n = st.staged.len();
    let unstaged_n = st.unstaged.len();
    let conflicts = !st.conflicts.is_empty();
    let branch = st.head.branch.clone();
    egui::Panel::bottom("commit_box")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 10)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            let te = egui::TextEdit::multiline(&mut rv.commit_msg)
                .hint_text("Describe your change in one line\n\nAdd more detail below if you like…")
                .desired_rows(3)
                .desired_width(f32::INFINITY);
            let r = ui.add(te);
            let submit = r.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
            ui.add_space(4.0);
            let has_msg = !rv.commit_msg.trim().is_empty();
            // On narrow panels the checkbox gets its own line so it never sits under the buttons.
            let narrow = ui.available_width() < 480.0;
            if narrow {
                amend_checkbox(ui, rv.amend, actions);
            }
            ui.horizontal(|ui| {
                if !narrow {
                    amend_checkbox(ui, rv.amend, actions);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let can = has_msg && (staged_n > 0 || rv.amend) && !conflicts;
                    let label = match (&branch, staged_n) {
                        (_, 0) if rv.amend => "Amend commit".to_string(),
                        (Some(b), n) => format!("Commit {n} file{} to {b}", if n == 1 { "" } else { "s" }),
                        (None, n) => format!("Commit {n} file{}", if n == 1 { "" } else { "s" }),
                    };
                    let btn = egui::Button::new(RichText::new(label).strong().color(if can { Color32::WHITE } else { DIM })).fill(if can { Color32::from_rgb(40, 130, 90) } else { RAISED });
                    let tip = if !has_msg { "Write a message first" } else if staged_n == 0 { "Stage some files first" } else { "Ctrl+Enter" };
                    let msg = rv.commit_msg.clone();
                    let amend = rv.amend;
                    if (ui.add_enabled(can, btn).on_hover_text(tip).on_disabled_hover_text(tip).clicked() || (submit && can)) && can {
                        let what = format!("{} “{}”", if amend { "amend" } else { "commit" }, msg.lines().next().unwrap_or("").trim());
                        actions.push(Action::Git {
                            repo: path.clone(),
                            label: if amend { "Amend commit".into() } else { "Commit".into() },
                            op: Box::new(move |p| git::commit(p, &msg, amend)),
                            kind: OpKind::Commit,
                            record: Some(Record { undo: git::Undoable::Refs, what }),
                        });
                    }
                    if staged_n == 0 && unstaged_n > 0 && !rv.amend {
                        let msg = rv.commit_msg.clone();
                        if ui.add_enabled(has_msg && !conflicts, egui::Button::new("Stage all & commit")).on_hover_text("Include every change in this commit").clicked() {
                            let what = format!("commit “{}”", msg.lines().next().unwrap_or("").trim());
                            actions.push(Action::Git {
                                repo: path.clone(),
                                label: "Commit".into(),
                                op: Box::new(move |p| {
                                    git::run(p, &["add", "-A"])?;
                                    git::commit(p, &msg, false)
                                }),
                                kind: OpKind::Commit,
                                record: Some(Record { undo: git::Undoable::Refs, what }),
                            });
                        }
                    }
                });
            });
        });

    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
        if st.change_count() == 0 {
            ui.add_space(70.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("✔").size(34.0).color(ACCENT));
                ui.label(RichText::new("Nothing to commit").size(16.0).strong());
                ui.label(RichText::new("Your files match the latest commit.").color(DIM));
            });
            return;
        }
        let Some(doc) = &rv.wd_doc else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().color(ACCENT)));
            return;
        };
        let fold = fold_bar(ui, doc);
        line_bar(ui, doc, &mut rv.line_sel, &path, actions);
        let toggled = diff_view(ui, doc, DiffMode::Unstaged, &mut rv.diff_scroll_to, Some(&mut rv.line_sel), &path, actions);
        // Fold/unfold, remembering the choice so it survives reloads after staging.
        if let Some(doc) = rv.wd_doc.as_mut() {
            let changed: Vec<usize> = match (fold, toggled) {
                (Some(all), _) => {
                    doc.set_all_collapsed(all);
                    (0..doc.files.len()).collect()
                }
                (None, Some(fi)) => {
                    doc.toggle(fi);
                    vec![fi]
                }
                _ => vec![],
            };
            if !changed.is_empty() {
                rv.line_sel = LineSel::default();
            }
            for fi in changed {
                rv.wd_folds.insert(doc.fold_key(fi), doc.is_collapsed(fi));
            }
        }
    });
}

fn amend_checkbox(ui: &mut Ui, amend: bool, actions: &mut Vec<Action>) {
    let mut amend = amend;
    if ui.checkbox(&mut amend, "Amend last commit").on_hover_text("Replace the previous commit instead of adding a new one").changed() {
        actions.push(Action::ToggleAmend);
    }
}

fn quiet(a: Action) -> Action {
    match a {
        Action::Git { repo, label, op, record, .. } => Action::Git { repo, label, op, kind: OpKind::Quiet, record },
        other => other,
    }
}

fn stage_all(path: &Path) -> Action {
    quiet(Action::args(path, "Stage all", &["add", "-A"]))
}

#[derive(Clone, Copy)]
enum ListKind {
    Conflicts,
    Unstaged,
    Staged,
}

/// A section title in the changes list, one row tall. Returns true when its button is clicked.
fn header_row(ui: &mut Ui, title: &str, n: usize, color: Color32, button: Option<&str>) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let t = p.layout_no_wrap(title.to_string(), egui::FontId::proportional(11.5), DIM);
    let tw = t.size().x;
    p.galley(pos2(rect.left(), y - t.size().y / 2.0), t, DIM);
    let c = p.layout_no_wrap(n.to_string(), theme::prop(12.0), color);
    let pr = Rect::from_min_size(pos2(rect.left() + tw + 8.0, y - 9.0), vec2(c.size().x + 16.0, 18.0));
    p.rect_filled(pr, 9.0, color.gamma_multiply(0.16));
    p.galley(pos2(pr.left() + 8.0, y - c.size().y / 2.0), c, color);
    let Some(b) = button else { return false };
    let g = p.layout_no_wrap(b.to_string(), theme::prop(12.5), TEXT);
    let w = g.size().x + 18.0;
    let br = Rect::from_min_size(pos2(rect.right() - w, rect.top() + 2.0), vec2(w, 20.0));
    row_button_sized(ui, br, b, false, RAISED, ("list_header_btn", title), 12.5).clicked()
}

/// A file row with small buttons that appear on the right. Returns (row, clicked button).
fn file_row_buttons(ui: &mut Ui, f: &FileChange, selected: bool, buttons: &[&str]) -> (egui::Response, Option<usize>) {
    let resp = file_line_tagged(ui, &f.path, f.old_path.as_deref(), &f.change, selected, None, f.submodule.then_some("submodule"));
    let mut clicked = None;
    let show = resp.hovered() || selected || ui.rect_contains_pointer(resp.rect);
    if show {
        let mut x = resp.rect.right() - 4.0;
        for (i, b) in buttons.iter().enumerate().rev() {
            let g = ui.painter().layout_no_wrap(b.to_string(), theme::prop(12.0), TEXT);
            let w = g.size().x + 22.0;
            let r = Rect::from_min_size(pos2(x - w, resp.rect.top() + 3.0), vec2(w, resp.rect.height() - 6.0));
            let danger = matches!(*b, "Discard" | "Delete");
            if row_button_sized(ui, r, b, danger, RAISED, ("list_btn", &f.path, *b), 12.0).clicked() {
                clicked = Some(i);
            }
            x -= w + 4.0;
        }
    }
    (resp, clicked)
}

/// Blame and history entries for a file's right-click menu. `at` is the commit the
/// file is shown in (`None`: the working copy); `change` is what happened to it there.
pub(crate) fn file_history_menu(ui: &mut Ui, path: &str, at: Option<&str>, change: &Change, actions: &mut Vec<Action>) {
    let rev = match (at, change) {
        // A deleted file is blamed as it was just before.
        (Some(h), Change::Deleted) => Some(Some(format!("{h}^"))),
        (Some(h), _) => Some(Some(h.to_string())),
        (None, Change::Untracked | Change::Added) => None,
        (None, Change::Deleted) => Some(Some("HEAD".to_string())),
        (None, _) => Some(None),
    };
    let blame = ui.add_enabled(rev.is_some(), egui::Button::new("Blame")).on_hover_text("Who last changed each line").on_disabled_hover_text("A new file has no history to blame yet");
    if blame.clicked() {
        actions.push(Action::File(FileOpen::Blame { rev: rev.flatten(), path: path.to_string(), line: None, back: false }));
    }
    if ui.button("History of this file").on_hover_text("Every commit that changed it, following renames").clicked() {
        actions.push(Action::File(FileOpen::History { path: path.to_string(), folder: false }));
    }
    if let Some((dir, _)) = path.rsplit_once('/')
        && ui.button(format!("History of {dir}/")).on_hover_text("Every commit that changed something in this folder").clicked()
    {
        actions.push(Action::File(FileOpen::History { path: dir.to_string(), folder: true }));
    }
}

/// Lines picked in a commit's diff: offer their history.
fn commit_line_bar(ui: &mut Ui, doc: &DiffDoc, sel: &mut LineSel, hash: &str, actions: &mut Vec<Action>) {
    if sel.rows.is_empty() {
        return;
    }
    if !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        sel.rows.clear();
        return;
    }
    let mut fi = None;
    let (mut new, mut old) = (Vec::new(), Vec::new());
    for &r in &sel.rows {
        if let Some(FlatRow::Line(f, hi, li)) = doc.rows.get(r) {
            fi = Some(*f);
            let l = &doc.files[*f].hunks[*hi].lines[*li];
            new.extend(l.new_no);
            old.extend(l.old_no.filter(|_| l.kind == LineKind::Del));
        }
    }
    let Some(fi) = fi else { return };
    let file = &doc.files[fi];
    // Added lines are found in this commit; only removed ones, in the one before.
    let (rev, path, nums) = if !new.is_empty() {
        (hash.to_string(), file.path.clone(), new)
    } else {
        (format!("{hash}^"), file.old_path.clone().unwrap_or_else(|| file.path.clone()), old)
    };
    let (a, z) = (*nums.iter().min().unwrap_or(&1), *nums.iter().max().unwrap_or(&1));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(32, 48, 66));
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut child;
    let n = sel.rows.len();
    ui.label(RichText::new(format!("{n} line{} picked", if n == 1 { "" } else { "s" })).strong().color(TEXT));
    ui.label(RichText::new(format!("in {}", file.path)).color(DIM));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button("Clear").on_hover_text("Esc").clicked() {
            sel.rows.clear();
        }
        let label = if a == z { "History of this line" } else { "History of these lines" };
        if ui.button(label).on_hover_text(format!("Every commit that changed {path} lines {a}–{z}")).clicked() {
            actions.push(Action::File(FileOpen::Lines { rev: Some(rev), path, start: a, end: z }));
        }
    });
}

// ---------------------------------------------------------------------------
// Diff viewer

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DiffMode {
    Commit,
    Unstaged,
    Staged,
    Untracked,
}

pub const DIFF_ROW: f32 = 19.0;

/// Longest part of a line that gets drawn. Laying out a huge line (minified code,
/// data files) every frame would freeze the window.
pub const MAX_LINE_CHARS: usize = 1000;

pub fn display_line(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_LINE_CHARS).collect::<String>().replace('\t', "    ");
    if text.len() > out.len() && text.chars().nth(MAX_LINE_CHARS).is_some() {
        out.push_str("  … (line shortened)");
    }
    out
}

/// A small button drawn to exactly fill `rect`, so it lines up with the row it sits in
/// (regular buttons have a minimum height taller than a diff row).
fn row_button(ui: &mut Ui, rect: Rect, text: &str, danger: bool, fill: Color32, id: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
    row_button_sized(ui, rect, text, danger, fill, id, 11.5)
}

fn row_button_sized(
    ui: &mut Ui,
    rect: Rect,
    text: &str,
    danger: bool,
    fill: Color32,
    id: impl std::hash::Hash + std::fmt::Debug,
    font_size: f32,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    name_it(&resp, egui::Role::Button, text);
    let p = ui.painter();
    let (bg, stroke) = if resp.is_pointer_button_down_on() {
        (Color32::from_rgb(60, 68, 80), Color32::from_rgb(80, 90, 104))
    } else if resp.hovered() {
        (Color32::from_rgb(52, 59, 69), Color32::from_rgb(70, 78, 90))
    } else {
        (fill, BORDER)
    };
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    p.text(rect.center(), Align2::CENTER_CENTER, text, theme::prop(font_size), if danger { DANGER } else { TEXT });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// What a button on an uncommitted file's name bar does.
fn file_action(repo: &Path, button: &str, f: git::FileChange) -> Action {
    let path = f.path.clone();
    match button {
        "Stage" => quiet(Action::git(repo, format!("Stage {path}"), move |r| git::stage(r, &[path]))),
        "Unstage" => {
            let mut paths = vec![path.clone()];
            if let Some(o) = &f.old_path {
                paths.push(o.clone());
            }
            quiet(Action::git(repo, format!("Unstage {path}"), move |r| git::unstage(r, &paths)))
        }
        "Open submodule" => Action::OpenRepo(repo.join(&path)),
        _ => {
            // Discard / Delete / Reset
            let what = if f.submodule {
                "put this submodule back on the commit recorded here"
            } else if f.change == Change::Untracked {
                "delete this new file"
            } else {
                "throw away your changes to this file"
            };
            Action::confirm(
                "Discard changes",
                format!("Really {what}?\n\n{path}\n\n{}", discard_note(&f)),
                "Discard",
                true,
                discard_file(repo, f),
            )
        }
    }
}

/// Discard a whole file, saving it first so the discard can be undone (not for
/// submodules: putting one back moves the submodule, not a file).
fn discard_file(repo: &Path, f: FileChange) -> Action {
    let path = f.path.clone();
    let a = Action::git(repo, format!("Discard {path}"), {
        let f = f.clone();
        move |r| git::discard(r, &f)
    });
    if f.submodule {
        return a;
    }
    let what = if f.change == Change::Untracked { format!("delete {path}") } else { format!("discard changes to {path}") };
    a.undoable(git::Undoable::Files(vec![path]), what)
}

fn discard_note(f: &FileChange) -> &'static str {
    if f.submodule { "This can't be undone." } else { "You can undo this afterwards (Ctrl+Z)." }
}

/// "N files · +a −r" with Collapse all / Expand all. Returns Some(fold everything?) when clicked.
fn fold_bar(ui: &mut Ui, doc: &DiffDoc) -> Option<bool> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, PANEL);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut child;
    let n = doc.files.len();
    let (a, r) = doc.files.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    ui.label(RichText::new(format!("{n} file{}", if n == 1 { "" } else { "s" })).strong());
    ui.label(RichText::new(format!("+{a}")).color(ADD_FG));
    ui.label(RichText::new(format!("−{r}")).color(DEL_FG));
    let mut out = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| out = fold_buttons(ui, doc));
    out
}

/// The "Collapse all" and "Expand all" buttons. Returns Some(true) to fold all, Some(false) to unfold all.
fn fold_buttons(ui: &mut Ui, doc: &DiffDoc) -> Option<bool> {
    let mut out = None;
    let all_open = doc.collapsed.is_empty();
    let all_folded = doc.collapsed.len() == doc.files.len();
    if ui.add_enabled(!all_open, egui::Button::new("Expand all").small()).on_hover_text("Show every file's changes").clicked() {
        out = Some(false);
    }
    if ui.add_enabled(!all_folded, egui::Button::new("Collapse all").small()).on_hover_text("Fold every file down to its name").clicked() {
        out = Some(true);
    }
    out
}

/// The bar above an uncommitted file's diff: how many lines are picked and what to do with them.
fn line_bar(ui: &mut Ui, doc: &DiffDoc, sel: &mut LineSel, repo: &Path, actions: &mut Vec<Action>) {
    if !(0..doc.files.len()).any(|fi| doc.editable(fi) && !doc.files[fi].hunks.is_empty()) {
        return;
    }
    if !sel.rows.is_empty() && !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        sel.rows.clear();
    }
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let fill = if sel.rows.is_empty() { PANEL } else { Color32::from_rgb(32, 48, 66) };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    let ui = &mut child;
    let n = sel.rows.len();
    if n == 0 {
        ui.label(RichText::new("Click a changed line to pick it · Shift-click or drag to pick several · then stage just those lines").size(12.0).color(FAINT));
        return;
    }
    // Picks always belong to one file (picking elsewhere starts over).
    let Some(fi) = sel.rows.iter().find_map(|&r| match doc.rows.get(r) {
        Some(FlatRow::Line(fi, ..)) => Some(*fi),
        _ => None,
    }) else {
        return;
    };
    let mode = doc.modes[fi];
    let picked: git::LineSelection = sel
        .rows
        .iter()
        .filter_map(|&r| match doc.rows.get(r) {
            Some(FlatRow::Line(f, hi, li)) if *f == fi => Some((*hi, *li)),
            _ => None,
        })
        .collect();
    let n = picked.len();
    let file = doc.files[fi].clone();
    ui.label(RichText::new(format!("{n} line{} picked", if n == 1 { "" } else { "s" })).strong().color(TEXT));
    ui.label(RichText::new(format!("in {}", file.path)).color(DIM));
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui.button("Clear").on_hover_text("Esc").clicked() {
            sel.rows.clear();
        }
        match mode {
            DiffMode::Unstaged | DiffMode::Untracked => {
                if mode == DiffMode::Unstaged && ui.button(RichText::new("Discard lines").color(DANGER)).clicked() {
                    let (f, p) = (file.clone(), picked.clone());
                    actions.push(Action::confirm(
                        "Discard lines",
                        format!("Throw away the changes on the {n} picked line{} from your file? You can undo this afterwards (Ctrl+Z).", if n == 1 { "" } else { "s" }),
                        "Discard",
                        true,
                        Action::git(repo, "Discard lines", move |r| git::discard_lines(r, &f, &p))
                            .undoable(git::Undoable::Files(vec![file.path.clone()]), format!("discard {n} line{} in {}", if n == 1 { "" } else { "s" }, file.path)),
                    ));
                }
                let untracked = mode == DiffMode::Untracked;
                let btn = egui::Button::new(RichText::new("Stage lines").strong().color(Color32::WHITE)).fill(Color32::from_rgb(40, 130, 90));
                if ui.add(btn).on_hover_text("Add only the picked lines to the next commit").clicked() {
                    actions.push(quiet(Action::git(repo, "Stage lines", move |r| git::stage_lines(r, &file, &picked, untracked))));
                }
            }
            DiffMode::Staged => {
                if ui.add(egui::Button::new(RichText::new("Unstage lines").strong())).on_hover_text("Take only the picked lines out of the next commit").clicked() {
                    actions.push(quiet(Action::git(repo, "Unstage lines", move |r| git::unstage_lines(r, &file, &picked))));
                }
            }
            DiffMode::Commit => {}
        }
    });
}

/// Pick lines with clicks and drags (only rows of changed lines count).
/// What the mouse did to a diff row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickEvent {
    /// A drag began on this row.
    DragStart,
    /// The pointer is over this row while a drag is in progress.
    DragOver,
    Click { shift: bool, command: bool },
}

/// Update the picked lines for one mouse event. Only rows of changed lines can be picked;
/// ranges skip over unchanged rows.
pub fn apply_pick(doc: &DiffDoc, sel: &mut LineSel, ri: usize, event: PickEvent) {
    let changed = |r: usize| match doc.rows.get(r) {
        Some(FlatRow::Line(fi, hi, li)) => matches!(doc.files[*fi].hunks[*hi].lines[*li].kind, LineKind::Add | LineKind::Del),
        _ => false,
    };
    let file_of = |r: usize| match doc.rows.get(r) {
        Some(FlatRow::Line(fi, ..)) => Some(*fi),
        _ => None,
    };
    // Ranges stay inside the file where they started.
    let range = |a: usize, b: usize| {
        (a.min(b)..=a.max(b)).filter(|&r| changed(r) && file_of(r) == file_of(a)).collect::<std::collections::BTreeSet<usize>>()
    };
    match event {
        PickEvent::DragStart => {
            sel.anchor = Some(ri);
            sel.dragging = true;
            sel.rows = range(ri, ri);
        }
        PickEvent::DragOver => {
            if let (true, Some(a)) = (sel.dragging, sel.anchor) {
                sel.rows = range(a, ri);
            }
        }
        PickEvent::Click { shift: true, .. } if sel.anchor.is_some() => {
            sel.rows = range(sel.anchor.unwrap(), ri);
        }
        PickEvent::Click { command: true, .. } => {
            // Picks from another file are dropped: one file at a time.
            if sel.rows.iter().any(|&r| file_of(r) != file_of(ri)) {
                sel.rows.clear();
            }
            if !sel.rows.remove(&ri) && changed(ri) {
                sel.rows.insert(ri);
            }
            sel.anchor = Some(ri);
        }
        PickEvent::Click { .. } => {
            if sel.rows.len() == 1 && sel.rows.contains(&ri) {
                sel.rows.clear();
            } else {
                sel.rows = range(ri, ri);
                sel.anchor = Some(ri);
            }
        }
    }
}

fn handle_line_pick(ui: &Ui, doc: &DiffDoc, ri: usize, resp: &egui::Response, sel: &mut LineSel) {
    let mods = ui.input(|i| i.modifiers);
    if resp.drag_started() {
        apply_pick(doc, sel, ri, PickEvent::DragStart);
    } else if resp.clicked() {
        apply_pick(doc, sel, ri, PickEvent::Click { shift: mods.shift, command: mods.command });
    }
    if sel.dragging {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            if resp.rect.y_range().contains(pos.y) {
                apply_pick(doc, sel, ri, PickEvent::DragOver);
            }
        }
    }
}

/// Space between the "before" and "after" pictures.
const IMAGE_GAP: f32 = 24.0;

/// Draw one strip of a changed picture. The diff draws only the rows on screen,
/// so each row paints its own slice: strip 0 has the titles, strip `k` the
/// picture's rows `(k - 1) * DIFF_ROW ..`.
fn paint_image_strip(ui: &Ui, rect: Rect, fi: usize, k: usize, pv: &preview::ImagePreview) {
    let clip = ui.clip_rect();
    let p = ui.painter().with_clip_rect(clip.intersect(rect));
    // Pinned to the visible left edge (like the section titles), and shrunk to fit a narrow pane.
    let left = clip.left() + 16.0;
    let n = pv.sides.len() as f32;
    let box_w = ((clip.width() - 32.0 - IMAGE_GAP * (n - 1.0)) / n).clamp(60.0, preview::BOX_W);
    for (i, side) in pv.sides.iter().enumerate() {
        let x = left + i as f32 * (box_w + IMAGE_GAP);
        if k == 0 {
            let title = if side.error.is_some() { side.label.to_string() } else { format!("{}  ·  {} × {}", side.label, side.size[0], side.size[1]) };
            // Named for screen readers (and tests): the picture's title.
            let r = Rect::from_min_size(pos2(x, rect.top()), vec2(box_w, DIFF_ROW));
            name_it(&ui.interact(r, ui.scope_id().with(("diff_image", fi, i)), Sense::hover()), egui::Role::Image, &title);
            p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, title, theme::prop(11.5), DIM);
            continue;
        }
        if let Some(e) = &side.error {
            if k == 1 {
                p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, e, theme::prop(12.0), FAINT);
            }
            continue;
        }
        // Where the whole picture sits, given that this strip shows part of it.
        let size = side.display_size();
        let size = size * (box_w / size.x).min(1.0);
        let full = Rect::from_min_size(pos2(x, rect.top() - (k - 1) as f32 * DIFF_ROW), size);
        if !full.intersects(rect) {
            continue;
        }
        preview::paint(&p, ui.ctx(), side, full, &format!("diff-image-{fi}-{i}"));
    }
}

pub fn diff_view(
    ui: &mut Ui,
    doc: &DiffDoc,
    mode: DiffMode,
    scroll_to: &mut Option<usize>,
    mut line_sel: Option<&mut LineSel>,
    repo: &Path,
    actions: &mut Vec<Action>,
) -> Option<usize> {
    // The commit shown (`None`: uncommitted changes), for blame and history.
    let at = doc.commit.as_deref();
    let mut toggled = None;
    if let Some(sel) = line_sel.as_deref_mut() {
        if sel.dragging && !ui.input(|i| i.pointer.primary_down()) {
            sel.dragging = false;
        }
    }
    if doc.files.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| ui.label(RichText::new("No differences to show").color(DIM)));
        return None;
    }
    let char_w = ui.fonts_mut(|f| f.glyph_width(&theme::mono(12.5), 'M'));
    let gutter = 92.0;
    let content_w = (gutter + 26.0 + doc.max_cols as f32 * char_w + 20.0).max(ui.available_width());
    let mut sa = egui::ScrollArea::both().auto_shrink(false).id_salt(("diff", repo, mode as u8));
    if let Some(r) = scroll_to.take() {
        // Never ask for more than the view can scroll, or it overshoots and slides back.
        let furthest = (doc.rows.len() as f32 * DIFF_ROW - ui.available_height()).max(0.0);
        sa = sa.vertical_scroll_offset((r as f32 * DIFF_ROW).min(furthest));
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, DIFF_ROW, doc.rows.len(), |ui, range| {
            ui.set_min_width(content_w);
            let clip = ui.clip_rect();
            for ri in range {
                let (rect, _) = ui.allocate_exact_size(vec2(content_w, DIFF_ROW), Sense::hover());
                let p = ui.painter().clone();
                let y = rect.center().y;
                let tx = rect.left() + gutter + 22.0;
                match doc.rows[ri] {
                    FlatRow::Section(si) => {
                        let (first, title) = &doc.sections[si];
                        let count = doc.sections.get(si + 1).map(|(next, _)| *next).unwrap_or(doc.files.len()) - first;
                        let vis = Rect::from_x_y_ranges(clip.x_range(), rect.y_range());
                        p.rect_filled(vis, 0.0, BG);
                        let color = if title == "STAGED" { ACCENT } else { WARN };
                        p.rect_filled(Rect::from_min_size(pos2(vis.left() + 8.0, y - 5.0), vec2(3.0, 10.0)), 1.0, color);
                        p.text(
                            pos2(vis.left() + 18.0, y),
                            Align2::LEFT_CENTER,
                            format!("{title}  ·  {count} file{}", if count == 1 { "" } else { "s" }),
                            theme::prop(11.5),
                            DIM,
                        );
                    }
                    FlatRow::File(fi) => {
                        let f = &doc.files[fi];
                        let vis = Rect::from_x_y_ranges(clip.x_range(), rect.y_range());
                        // The name bar folds/unfolds this file's changes.
                        let hdr = ui.interact(vis, ui.scope_id().with(("file_header", fi)), Sense::click());
                        name_it(&hdr, egui::Role::Button, format!("{} {}", if doc.is_collapsed(fi) { "Unfold" } else { "Fold" }, f.path));
                        let folded = doc.is_collapsed(fi);
                        p.rect_filled(vis, 0.0, if hdr.hovered() { Color32::from_rgb(52, 59, 69) } else { RAISED });
                        if hdr.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        paint_chevron(&p, pos2(vis.left() + 12.0, y), !folded, if hdr.hovered() { TEXT } else { DIM });
                        let cc = change_color(&f.change);
                        p.text(pos2(vis.left() + 24.0, y), Align2::LEFT_CENTER, f.change.letter(), theme::mono(12.5), cc);
                        let name = match &f.old_path {
                            Some(o) => format!("{o}  →  {}", f.path),
                            None => f.path.clone(),
                        };
                        let name_x = vis.left() + 42.0;
                        let hdr = hdr.on_hover_text(if folded { "Click to show this file's changes" } else { "Click to fold this file's changes" });
                        if hdr.clicked() {
                            toggled = Some(fi);
                        }
                        hdr.context_menu(|ui| file_history_menu(ui, &f.path, at, &f.change, actions));
                        // Buttons for uncommitted files (added after the bar, so they sit on top).
                        let sub = doc.submodule.get(fi).copied().unwrap_or(false);
                        let buttons: &[&str] = match (doc.modes[fi], sub) {
                            (DiffMode::Unstaged, true) => &["Open submodule", "Stage", "Reset"],
                            (DiffMode::Staged, true) => &["Open submodule", "Unstage"],
                            (DiffMode::Unstaged, false) => &["Stage", "Discard"],
                            (DiffMode::Untracked, _) => &["Stage", "Delete"],
                            (DiffMode::Staged, false) => &["Unstage"],
                            (DiffMode::Commit, _) => &[],
                        };
                        let mut x = vis.right() - 6.0;
                        for b in buttons.iter().rev() {
                            let g = p.layout_no_wrap(b.to_string(), theme::prop(11.5), TEXT);
                            let w = g.size().x + 22.0;
                            let br = Rect::from_min_size(pos2(x - w, rect.top() + 1.0), vec2(w, DIFF_ROW - 2.0));
                            x -= w + 4.0;
                            let danger = matches!(*b, "Discard" | "Delete");
                            if row_button(ui, br, b, danger, PANEL, ("file_btn", fi, *b)).clicked() {
                                let fc = git::FileChange { path: f.path.clone(), old_path: f.old_path.clone(), change: f.change.clone(), submodule: sub };
                                actions.push(file_action(repo, b, fc));
                            }
                        }
                        // Counts sit left of the buttons; the name gets the rest and is cut with "…" if needed.
                        let stats = p.layout_no_wrap(format!("+{}  −{}", f.added, f.removed), theme::prop(12.0), DIM);
                        let stats_x = x - 6.0 - stats.size().x;
                        p.galley(pos2(stats_x, y - stats.size().y / 2.0), stats, DIM);
                        let g = trunc(&p, &name, theme::mono(12.5), TEXT, stats_x - 12.0 - name_x);
                        p.galley(pos2(name_x, y - g.size().y / 2.0), g, TEXT);
                    }
                    FlatRow::Note(fi) => {
                        let f = &doc.files[fi];
                        let t = match &f.note {
                            Some(n) => n.as_str(),
                            None if f.binary => "Binary file — contents not shown",
                            None => "No text changes (mode or empty file)",
                        };
                        p.text(pos2(tx, y), Align2::LEFT_CENTER, t, theme::prop(12.5), DIM);
                    }
                    FlatRow::Image(fi, k) => {
                        if let Some(pv) = doc.preview(fi) {
                            paint_image_strip(ui, rect, fi, k, pv);
                        }
                    }
                    FlatRow::Gap => {}
                    FlatRow::Hunk(fi, hi) => {
                        let h = &doc.files[fi].hunks[hi];
                        p.rect_filled(rect, 0.0, HUNK_BG);
                        p.text(pos2(rect.left() + 8.0, y), Align2::LEFT_CENTER, &h.header, theme::mono(12.0), Color32::from_rgb(120, 150, 190));
                        // Each file decides its own buttons (the stacked view mixes kinds).
                        let mode = if doc.editable(fi) { doc.modes[fi] } else { DiffMode::Commit };
                        let buttons: &[&str] = match mode {
                            DiffMode::Unstaged => &["Stage hunk", "Discard hunk"],
                            DiffMode::Staged => &["Unstage hunk"],
                            _ => &[],
                        };
                        let mut x = clip.right() - 6.0;
                        for (bi, b) in buttons.iter().enumerate().rev() {
                            let g = p.layout_no_wrap(b.to_string(), theme::prop(11.5), TEXT);
                            let w = g.size().x + 22.0;
                            let br = Rect::from_min_size(pos2(x - w, rect.top() + 1.0), vec2(w, DIFF_ROW - 2.0));
                            x -= w + 4.0;
                            if row_button(ui, br, b, bi == 1, RAISED, ("hunk_btn", fi, hi, *b)).clicked() {
                                let Some(patch) = doc.files[fi].hunk_patch(hi) else { continue };
                                let file_path = doc.files[fi].path.clone();
                                match (mode, bi) {
                                    (DiffMode::Unstaged, 0) => actions.push(quiet(Action::git(repo, "Stage hunk", move |r| git::apply_hunk(r, &patch, false, true)))),
                                    (DiffMode::Unstaged, _) => actions.push(Action::confirm(
                                        "Discard hunk",
                                        "Throw away this block of changes from your file? You can undo this afterwards (Ctrl+Z).".into(),
                                        "Discard",
                                        true,
                                        Action::git(repo, "Discard hunk", move |r| git::apply_hunk(r, &patch, true, false))
                                            .undoable(git::Undoable::Files(vec![file_path.clone()]), format!("discard hunk in {file_path}")),
                                    )),
                                    _ => actions.push(quiet(Action::git(repo, "Unstage hunk", move |r| git::apply_hunk(r, &patch, true, true)))),
                                }
                            }
                        }
                    }
                    FlatRow::Line(fi, hi, li) => {
                        let l = &doc.files[fi].hunks[hi].lines[li];
                        let changed_line = matches!(l.kind, LineKind::Add | LineKind::Del);
                        let mut picked = false;
                        let mut hovered = false;
                        if let Some(sel) = line_sel.as_deref_mut().filter(|_| doc.editable(fi) || doc.modes[fi] == DiffMode::Commit) {
                            let resp = ui.interact(rect, ui.scope_id().with(("diff_line", ri)), Sense::click_and_drag());
                            name_it(&resp, egui::Role::Row, format!("line {}", l.text));
                            handle_line_pick(ui, doc, ri, &resp, sel);
                            picked = sel.rows.contains(&ri);
                            hovered = changed_line && resp.hovered();
                            if hovered {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                        }
                        let (bg, fg, mark) = match l.kind {
                            LineKind::Add => (ADD_BG, ADD_FG, "+"),
                            LineKind::Del => (DEL_BG, DEL_FG, "−"),
                            LineKind::Context => (Color32::TRANSPARENT, Color32::from_rgb(196, 202, 210), ""),
                            LineKind::NoNewline => (Color32::TRANSPARENT, FAINT, ""),
                        };
                        if bg != Color32::TRANSPARENT {
                            p.rect_filled(rect, 0.0, bg);
                        }
                        let gr = Rect::from_min_size(rect.min, vec2(gutter, DIFF_ROW));
                        p.rect_filled(gr, 0.0, Color32::from_black_alpha(40));
                        let nf = theme::mono(11.5);
                        if let Some(n) = l.old_no {
                            p.text(pos2(rect.left() + 40.0, y), Align2::RIGHT_CENTER, n.to_string(), nf.clone(), FAINT);
                        }
                        if let Some(n) = l.new_no {
                            p.text(pos2(rect.left() + 84.0, y), Align2::RIGHT_CENTER, n.to_string(), nf, FAINT);
                        }
                        if picked {
                            p.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(90, 160, 255, 48));
                            p.rect_filled(Rect::from_min_size(rect.min, vec2(4.0, DIFF_ROW)), 0.0, BLUE);
                        } else if hovered {
                            p.rect_filled(rect, 0.0, Color32::from_white_alpha(10));
                            p.rect_filled(Rect::from_min_size(rect.min, vec2(4.0, DIFF_ROW)), 0.0, BLUE.gamma_multiply(0.4));
                        }
                        p.text(pos2(rect.left() + gutter + 8.0, y), Align2::LEFT_CENTER, mark, theme::mono(12.5), fg);
                        let text = display_line(&l.text);
                        p.text(pos2(tx, y), Align2::LEFT_CENTER, text, theme::mono(12.5), fg);
                    }
                }
            }
        });
    });
    toggled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_patch;

    /// Rows: 0 hunk header, 1 " a", 2 "-b", 3 "+B", 4 " c", 5 "+d", 6 "+e", 7 hunk header, 8 "-x".
    fn doc() -> DiffDoc {
        let files = parse_patch(
            "diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,3 +1,5 @@
 a
-b
+B
 c
+d
+e
@@ -20,1 +22,0 @@
-x
",
        );
        DiffDoc::new(files, false)
    }

    fn click(shift: bool, command: bool) -> PickEvent {
        PickEvent::Click { shift, command }
    }

    fn picked(sel: &LineSel) -> Vec<usize> {
        sel.rows.iter().copied().collect()
    }

    #[test]
    fn click_picks_one_changed_line_and_click_again_unpicks() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 2, click(false, false));
        assert_eq!(picked(&sel), [2]);
        apply_pick(&d, &mut sel, 3, click(false, false));
        assert_eq!(picked(&sel), [3], "a plain click replaces the pick");
        apply_pick(&d, &mut sel, 3, click(false, false));
        assert!(sel.rows.is_empty());
    }

    #[test]
    fn unchanged_lines_and_headers_cannot_be_picked() {
        let d = doc();
        let mut sel = LineSel::default();
        for row in [0, 1, 4, 7] {
            apply_pick(&d, &mut sel, row, click(false, false));
            assert!(sel.rows.is_empty(), "row {row}");
        }
        apply_pick(&d, &mut sel, 1, click(false, true));
        assert!(sel.rows.is_empty());
    }

    #[test]
    fn shift_click_picks_a_range_skipping_unchanged_lines() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 2, click(false, false));
        apply_pick(&d, &mut sel, 8, click(true, false));
        assert_eq!(picked(&sel), [2, 3, 5, 6, 8]);
        // Ranges work upwards too, from the same anchor.
        apply_pick(&d, &mut sel, 1, click(true, false));
        assert_eq!(picked(&sel), [2]);
    }

    #[test]
    fn shift_click_without_an_anchor_acts_like_a_click() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 5, click(true, false));
        assert_eq!(picked(&sel), [5]);
    }

    #[test]
    fn ctrl_click_adds_and_removes_single_lines() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 2, click(false, true));
        apply_pick(&d, &mut sel, 6, click(false, true));
        apply_pick(&d, &mut sel, 8, click(false, true));
        assert_eq!(picked(&sel), [2, 6, 8]);
        apply_pick(&d, &mut sel, 6, click(false, true));
        assert_eq!(picked(&sel), [2, 8]);
    }

    #[test]
    fn dragging_picks_everything_between_start_and_pointer() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 3, PickEvent::DragStart);
        assert!(sel.dragging);
        assert_eq!(picked(&sel), [3]);
        apply_pick(&d, &mut sel, 6, PickEvent::DragOver);
        assert_eq!(picked(&sel), [3, 5, 6]);
        // Moving back shrinks the pick.
        apply_pick(&d, &mut sel, 4, PickEvent::DragOver);
        assert_eq!(picked(&sel), [3]);
        apply_pick(&d, &mut sel, 2, PickEvent::DragOver);
        assert_eq!(picked(&sel), [2, 3]);
        // Once the drag has ended, hovering changes nothing.
        sel.dragging = false;
        apply_pick(&d, &mut sel, 8, PickEvent::DragOver);
        assert_eq!(picked(&sel), [2, 3]);
    }

    #[test]
    fn picked_rows_map_to_the_right_patch_lines() {
        let d = doc();
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 3, click(false, false));
        apply_pick(&d, &mut sel, 8, click(false, true));
        let lines: git::LineSelection = sel
            .rows
            .iter()
            .filter_map(|&r| match d.rows[r] {
                FlatRow::Line(_, hi, li) => Some((hi, li)),
                _ => None,
            })
            .collect();
        assert_eq!(lines.into_iter().collect::<Vec<_>>(), [(0, 2), (1, 0)]);
    }

    #[test]
    fn picks_stay_inside_one_file() {
        // Two files: rows 0 file a, 1 hunk, 2 "-x", 3 "+y", 4 gap, 5 file b, 6 hunk, 7 "-p", 8 "+q", 9 gap.
        let files = parse_patch(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-p\n+q\n",
        );
        let d = DiffDoc::new(files, true);
        let mut sel = LineSel::default();
        apply_pick(&d, &mut sel, 2, click(false, false));
        apply_pick(&d, &mut sel, 8, click(true, false));
        assert_eq!(picked(&sel), [2, 3], "a shift-click range doesn't spill into the next file");
        apply_pick(&d, &mut sel, 7, click(false, true));
        assert_eq!(picked(&sel), [7], "ctrl-click in another file starts over there");
        apply_pick(&d, &mut sel, 3, PickEvent::DragStart);
        apply_pick(&d, &mut sel, 8, PickEvent::DragOver);
        assert_eq!(picked(&sel), [3], "dragging stays in the file where it began");
    }

    #[test]
    fn keys_move_the_history_selection() {
        // 10 rows, a page is 4 rows.
        let t = |cur, key| nav_target(cur, 10, key, 4);
        assert_eq!(t(3, NavKey::Down), Some(4));
        assert_eq!(t(3, NavKey::Up), Some(2));
        assert_eq!(t(3, NavKey::PageDown), Some(7));
        assert_eq!(t(7, NavKey::PageDown), Some(9), "stops at the last row");
        assert_eq!(t(3, NavKey::PageUp), Some(0), "stops at the first row");
        assert_eq!(t(5, NavKey::Home), Some(0));
        assert_eq!(t(5, NavKey::End), Some(9));
        // Already at an end: nothing moves.
        assert_eq!(t(0, NavKey::Up), None);
        assert_eq!(t(0, NavKey::PageUp), None);
        assert_eq!(t(9, NavKey::Down), None);
        assert_eq!(t(9, NavKey::End), None);
        assert_eq!(nav_target(0, 0, NavKey::Down, 4), None);
    }

    #[test]
    fn scrolling_follows_the_selection() {
        // Rows 10px tall, a 100px window scrolled to 50px shows rows 5..=14.
        assert_eq!(keep_visible(7, 50.0, 100.0, 10.0), None, "already visible");
        assert_eq!(keep_visible(14, 50.0, 100.0, 10.0), None, "last fully visible row");
        assert_eq!(keep_visible(15, 50.0, 100.0, 10.0), Some(60.0), "one below: scroll by a row");
        assert_eq!(keep_visible(40, 50.0, 100.0, 10.0), Some(310.0), "far below: bottom edge");
        assert_eq!(keep_visible(4, 50.0, 100.0, 10.0), Some(40.0), "one above: scroll up a row");
        assert_eq!(keep_visible(0, 50.0, 100.0, 10.0), Some(0.0));
        // Before the list has been measured, only scroll up.
        assert_eq!(keep_visible(30, 0.0, 0.0, 10.0), None);
    }

    #[test]
    fn very_long_lines_are_shortened_for_display() {
        assert_eq!(display_line("a\tb"), "a    b");
        let short = "x".repeat(MAX_LINE_CHARS);
        assert_eq!(display_line(&short), short);
        let long = "é".repeat(MAX_LINE_CHARS + 5);
        let shown = display_line(&long);
        assert!(shown.ends_with("(line shortened)"));
        assert_eq!(shown.chars().filter(|&c| c == 'é').count(), MAX_LINE_CHARS);
    }
}
