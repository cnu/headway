//! Keyboard navigation between and inside the repository screen's lists.
//!
//! One list at a time has the keyboard (the *pane*): the history (the default; its
//! arrow keys are handled where it's drawn), the files of the selected commit or
//! of the uncommitted changes, or the sidebar's branches. Enter on a history row
//! goes into its files, F6 moves to the next list and Escape comes back to the
//! history. In each list the arrow keys, Page Up/Down, Home and End move, and Enter
//! does what a double-click does. The selected row of the list that has the
//! keyboard gets a focus ring while the keyboard is in use.

use crate::actions::Id;
use crate::app::{Action, FileOpen, FileSel, Pane, RepoView, Selection};
use crate::git::{Branch, Change};
use crate::views::{self, NavKey, keep_visible, nav_target};
use egui::{Context, Key, Modifiers, Ui};

/// Height of one row of the changes list (see `views::staging_panel`).
pub const STAGE_ROW_H: f32 = 24.0;

/// The branches the sidebar lists, in its order.
pub fn sidebar_branches(rv: &RepoView) -> Vec<&Branch> {
    let Some(data) = &rv.data else { return Vec::new() };
    let merged = |b: &Branch| rv.prs.get(&b.name).is_some_and(|s| s.pr.state == crate::forge::PrState::Merged);
    data.branches.iter().filter(|b| !b.remote).filter(|b| !rv.merged_only || merged(b)).collect()
}

/// The files the arrow keys walk through in the uncommitted changes: unstaged, then staged.
pub fn change_files(rv: &RepoView) -> Vec<FileSel> {
    let Some(data) = &rv.data else { return Vec::new() };
    let st = &data.status;
    let unstaged = st.unstaged.iter().map(|f| FileSel { path: f.path.clone(), staged: false, untracked: f.change == Change::Untracked });
    let staged = st.staged.iter().map(|f| FileSel { path: f.path.clone(), staged: true, untracked: false });
    unstaged.chain(staged).collect()
}

/// Which row of the changes list (headers and spacers count) a file is on.
pub fn change_row(rv: &RepoView, sel: &FileSel) -> Option<usize> {
    let st = &rv.data.as_ref()?.status;
    let section = |n: usize| n.max(1) + 2;
    let mut row = 0;
    if !st.conflicts.is_empty() {
        row += section(st.conflicts.len());
    }
    if !sel.staged {
        return st.unstaged.iter().position(|f| f.path == sel.path).map(|i| row + 1 + i);
    }
    row += section(st.unstaged.len());
    st.staged.iter().position(|f| f.path == sel.path).map(|i| row + 1 + i)
}

/// Whether the keyboard has been used since the mouse last was (the focus ring shows then).
pub fn ring(ctx: &Context, rv: &RepoView, pane: Pane) -> bool {
    rv.pane == pane && crate::keys::keyboard_mode(ctx)
}

fn nav_key(ui: &mut Ui) -> Option<NavKey> {
    ui.input_mut(|i| {
        [
            (Key::ArrowUp, NavKey::Up),
            (Key::ArrowDown, NavKey::Down),
            (Key::PageUp, NavKey::PageUp),
            (Key::PageDown, NavKey::PageDown),
            (Key::Home, NavKey::Home),
            (Key::End, NavKey::End),
        ]
        .into_iter()
        .find(|(k, _)| i.consume_key(Modifiers::NONE, *k))
        .map(|(_, n)| n)
    })
}

/// Handle this frame's keys for the list that has them. Called before the screen is drawn.
pub fn keys(ui: &mut Ui, rv: &mut RepoView, sidebar_open: bool, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let busy = rv.modal || ctx.text_edit_focused() || ctx.any_popup_open() || rv.pr_keys;
    let other_screen = rv.resolver.as_ref().is_some_and(|r| r.shown) || rv.pr_view.as_ref().is_some_and(|p| p.shown) || rv.file_view.as_ref().is_some_and(|f| f.shown);
    if busy || other_screen || rv.data.is_none() {
        return;
    }
    // A list that's gone (no files left, the sidebar hidden) gives the keys back to the history.
    if (rv.pane == Pane::Files && !has_files(rv)) || (rv.pane == Pane::Branches && !sidebar_open) {
        rv.pane = Pane::History;
    }
    let escape = ctx.input(|i| i.key_pressed(Key::Escape) && !i.modifiers.any());
    match rv.pane {
        Pane::History => {
            // Enter goes into the files of what's selected.
            if enter_free(&ctx) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) && has_files(rv) {
                rv.pane = Pane::Files;
                if matches!(rv.selection, Some(Selection::Commit(_))) && rv.commit_file.is_none() {
                    rv.commit_file = Some(0);
                }
                // Nothing picked yet: pick the first file (the ring needs a row).
                if matches!(rv.selection, Some(Selection::WorkingDir) | None)
                    && rv.file_sel.is_none()
                    && let Some(first) = change_files(rv).into_iter().next()
                {
                    actions.push(Action::SelectFile(first));
                }
            }
        }
        Pane::Files => {
            if escape {
                rv.pane = Pane::History;
                return;
            }
            files_keys(ui, rv, actions);
        }
        Pane::Branches => {
            if escape {
                rv.pane = Pane::History;
                return;
            }
            branch_keys(ui, rv, actions);
        }
    }
}

fn has_files(rv: &RepoView) -> bool {
    match &rv.selection {
        Some(Selection::Commit(_)) => rv.detail.as_ref().is_some_and(|d| !d.doc.files.is_empty()),
        Some(Selection::WorkingDir) | None => !change_files(rv).is_empty(),
        Some(Selection::Reflog) => false,
    }
}

/// Enter belongs to a button (or any widget) that has the keyboard focus, not to a list.
pub fn enter_free(ctx: &Context) -> bool {
    ctx.memory(|m| m.focused().is_none())
}

fn files_keys(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let key = nav_key(ui);
    let enter = enter_free(ui.ctx()) && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    match rv.selection.clone() {
        Some(Selection::Commit(hash)) => {
            let n = rv.detail.as_ref().map_or(0, |d| d.doc.files.len());
            if n == 0 {
                return;
            }
            let cur = rv.commit_file.unwrap_or(0).min(n - 1);
            if let Some(t) = key.and_then(|k| nav_target(cur, n, k, 8)) {
                rv.commit_file = Some(t);
                // The same as clicking the file: unfold it and scroll the diff to it.
                if let Some(d) = rv.detail.as_mut() {
                    d.doc.set_collapsed(t, false);
                    rv.diff_scroll_to = d.doc.file_rows.get(t).copied();
                }
                rv.files_scroll = true;
            }
            if enter && let Some(f) = rv.detail.as_ref().and_then(|d| d.doc.files.get(cur)) {
                actions.push(Action::File(FileOpen::Blame { rev: Some(hash), path: f.path.clone(), line: None, back: false }));
            }
        }
        Some(Selection::WorkingDir) | None => {
            let list = change_files(rv);
            if list.is_empty() {
                return;
            }
            let cur = rv.file_sel.as_ref().and_then(|s| list.iter().position(|f| f.path == s.path && f.staged == s.staged)).unwrap_or(0);
            if let Some(t) = key.and_then(|k| nav_target(cur, list.len(), k, 8)) {
                let sel = list[t].clone();
                if let Some(row) = change_row(rv, &sel) {
                    let (offset, view_h) = rv.stage_view;
                    rv.stage_scroll_to = keep_visible(row, offset, view_h, STAGE_ROW_H);
                }
                actions.push(Action::SelectFile(sel));
            }
            // Enter is what a double-click is: stage it, or take it back out.
            if enter {
                actions.push(Action::Run(if list[cur].staged { Id::UnstageFile } else { Id::StageFile }));
            }
        }
        Some(Selection::Reflog) => {}
    }
}

fn branch_keys(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let key = nav_key(ui);
    let enter = enter_free(ui.ctx()) && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let names: Vec<String> = sidebar_branches(rv).iter().map(|b| b.name.clone()).collect();
    if names.is_empty() {
        return;
    }
    let head = sidebar_branches(rv).iter().position(|b| b.is_head).unwrap_or(0);
    let cur = rv.branch_sel.as_ref().and_then(|n| names.iter().position(|x| x == n)).unwrap_or(head);
    if let Some(t) = key.and_then(|k| nav_target(cur, names.len(), k, 8)) {
        rv.branch_sel = Some(names[t].clone());
        rv.branch_scroll = true;
        actions.push(Action::SelectRef(names[t].clone()));
    }
    if enter {
        // What a double-click does: switch to it (or go to the worktree that has it).
        let name = &names[cur];
        let is_head = sidebar_branches(rv).iter().any(|b| b.is_head && &b.name == name);
        if !is_head && let Some(data) = &rv.data {
            let other = views::branches_in_other_worktrees(data).get(name.as_str()).map(|p| p.to_path_buf());
            actions.push(match other {
                Some(p) => Action::OpenWorktree(p),
                None => views::checkout(&rv.path, name),
            });
        }
    }
}

/// Move the keyboard to the next (or previous) list.
pub fn cycle(rv: &mut RepoView, forward: bool, sidebar_open: bool) {
    let order: Vec<Pane> = [Pane::History, Pane::Files, Pane::Branches].into_iter().filter(|p| *p != Pane::Branches || sidebar_open).collect();
    let at = order.iter().position(|p| *p == rv.pane).unwrap_or(0);
    let to = if forward { (at + 1) % order.len() } else { (at + order.len() - 1) % order.len() };
    rv.pane = order[to];
    match rv.pane {
        Pane::Files => {
            if matches!(rv.selection, Some(Selection::Commit(_))) && rv.commit_file.is_none() {
                rv.commit_file = Some(0);
            }
        }
        Pane::Branches => {
            if rv.branch_sel.is_none() {
                rv.branch_sel = sidebar_branches(rv).iter().find(|b| b.is_head).map(|b| b.name.clone());
            }
        }
        Pane::History => {}
    }
}

/// Draw a focus ring around a row.
pub fn focus_ring(ui: &Ui, rect: egui::Rect) {
    ui.painter().rect_stroke(rect.shrink(0.5), 4.0, egui::Stroke::new(1.5, crate::theme::ACCENT), egui::StrokeKind::Inside);
}
