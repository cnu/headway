//! Blame and file history: who last changed each line of a file, and the commits
//! that touched a file, a folder or a few lines. Opens over the history and
//! details (like the conflict resolver) and can be put aside to look at a commit.

use crate::app::{Action, DiffDoc, FileOpen, RepoView};
use crate::git::{self, BlameCommit, Change, FileCommit};
use crate::highlight;
use crate::theme::{self, *};
use crate::views::{self, DIFF_ROW, DiffMode, NavKey, name_it, paint_row_bg, trunc};
use crate::workspace::ViewPrefs;
use egui::{Align, Align2, Color32, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

/// How many commits a file or folder history lists.
pub const HISTORY_LIMIT: usize = 2000;
/// How many commits line history lists.
pub const LINE_HISTORY_LIMIT: usize = 500;

const GUTTER_W: f32 = 290.0;
const NUM_W: f32 = 52.0;
const HISTORY_ROW: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Blame,
    History,
}

/// Where one line of the blamed file came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Owner {
    /// Index into [`Blame::commits`].
    pub commit: u32,
    /// The line's number in that commit's version of the file.
    pub orig_line: u32,
    /// Index into [`Blame::paths`]: the file's name in that commit.
    pub path: u32,
}

pub struct Blame {
    /// The commit blamed, or the working copy when `None`.
    pub rev: Option<String>,
    pub path: String,
    pub generation: u64,
    pub lines: Option<Result<Vec<String>, String>>,
    /// Who last changed each line (filled in as git finds them).
    pub owners: Vec<Option<Owner>>,
    pub commits: Vec<BlameCommit>,
    pub paths: Vec<String>,
    pub done: bool,
    pub error: Option<String>,
    pub ignore: Option<git::IgnoreRevs>,
    /// Oldest and newest author dates, for the age colours.
    span: (i64, i64),
    /// Picked lines (from 0): where the pick started and where it ends now.
    pub sel: Option<(usize, usize)>,
    dragging: bool,
    pub scroll_to: Option<usize>,
    /// Scroll position and visible height of the lines, as of the last frame.
    view: (f32, f32),
    /// The commit whose gutter the pointer is over (its lines are highlighted).
    hover: Option<u32>,
    max_cols: usize,
    /// Syntax colours of the file, once they're ready.
    pub syntax: Option<std::sync::Arc<highlight::Highlighted>>,
}

impl Blame {
    pub fn new(rev: Option<String>, path: String, generation: u64, line: Option<u32>) -> Self {
        let at = line.map(|l| l.saturating_sub(1) as usize);
        Self {
            rev,
            path,
            generation,
            lines: None,
            owners: Vec::new(),
            commits: Vec::new(),
            paths: Vec::new(),
            done: false,
            error: None,
            ignore: None,
            span: (i64::MAX, i64::MIN),
            sel: at.map(|l| (l, l)),
            dragging: false,
            // A few lines of context above the line being followed.
            scroll_to: at.map(|l| l.saturating_sub(5)),
            view: (0.0, 0.0),
            hover: None,
            max_cols: 40,
            syntax: None,
        }
    }

    pub fn text_loaded(&mut self, lines: Result<Vec<String>, String>, ignore: Option<git::IgnoreRevs>) {
        if let Ok(l) = &lines {
            self.owners = vec![None; l.len()];
            self.max_cols = l.iter().map(|t| t.len() + t.matches('\t').count() * 3).max().unwrap_or(0).clamp(40, views::MAX_LINE_CHARS + 24);
            if let Some((a, b)) = &mut self.sel {
                *a = (*a).min(l.len().saturating_sub(1));
                *b = (*b).min(l.len().saturating_sub(1));
            }
        } else {
            self.done = true;
        }
        self.lines = Some(lines);
        self.ignore = ignore;
    }

    /// More results from git.
    pub fn add(&mut self, commits: Vec<BlameCommit>, ranges: Vec<git::BlameRange>) {
        for c in &commits {
            if !c.uncommitted() {
                self.span = (self.span.0.min(c.time), self.span.1.max(c.time));
            }
        }
        self.commits.extend(commits);
        for r in ranges {
            let pi = match self.paths.iter().position(|p| *p == r.path) {
                Some(i) => i,
                None => {
                    self.paths.push(r.path);
                    self.paths.len() - 1
                }
            } as u32;
            for k in 0..r.count {
                let i = (r.line + k).saturating_sub(1) as usize;
                if let Some(o) = self.owners.get_mut(i) {
                    *o = Some(Owner { commit: r.commit as u32, orig_line: r.orig_line + k, path: pi });
                }
            }
        }
    }

    pub fn finished(&mut self, res: Result<(), String>) {
        self.done = true;
        self.error = res.err();
    }

    /// The picked lines (from 0), first to last.
    pub fn picked(&self) -> Option<(usize, usize)> {
        self.sel.map(|(a, b)| (a.min(b), a.max(b)))
    }

    /// Who last changed line `i` (from 0).
    pub fn owner(&self, i: usize) -> Option<(&BlameCommit, Owner)> {
        let o = self.owners.get(i).copied().flatten()?;
        Some((self.commits.get(o.commit as usize)?, o))
    }

    fn blamed(&self) -> usize {
        self.owners.iter().filter(|o| o.is_some()).count()
    }

    fn age_color(&self, c: &BlameCommit) -> Color32 {
        if c.uncommitted() { ACCENT } else { theme::age(theme::age_of(c.time, self.span.0, self.span.1)) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HistoryKind {
    File,
    Folder,
    /// Lines `start..=end` of the file at `rev` (the working copy when `None`).
    Lines { rev: Option<String>, start: u32, end: u32 },
}

pub struct History {
    pub kind: HistoryKind,
    pub path: String,
    pub generation: u64,
    /// The commits, each with its change to the lines (line history only).
    pub list: Option<Result<Vec<git::HistoryEntry>, String>>,
    pub selected: Option<usize>,
    pub diff: Option<Result<DiffDoc, String>>,
    pub diff_generation: u64,
    pub diff_scroll: Option<usize>,
    pub list_scroll_to: Option<usize>,
    list_view: (f32, f32),
}

impl History {
    pub fn new(kind: HistoryKind, path: String, generation: u64) -> Self {
        Self { kind, path, generation, list: None, selected: None, diff: None, diff_generation: 0, diff_scroll: None, list_scroll_to: None, list_view: (0.0, 0.0) }
    }

    pub fn commits(&self) -> &[git::HistoryEntry] {
        match &self.list {
            Some(Ok(l)) => l,
            _ => &[],
        }
    }

    pub fn selected_commit(&self) -> Option<&FileCommit> {
        self.commits().get(self.selected?).map(|(c, _)| c)
    }

    fn title(&self) -> String {
        match &self.kind {
            HistoryKind::File => self.path.clone(),
            HistoryKind::Folder => format!("{}/", self.path.trim_end_matches('/')),
            HistoryKind::Lines { start, end, .. } if start == end => format!("{} · line {start}", self.path),
            HistoryKind::Lines { start, end, .. } => format!("{} · lines {start}–{end}", self.path),
        }
    }
}

pub struct FileView {
    pub mode: Mode,
    pub blame: Option<Blame>,
    pub history: Option<History>,
    /// Earlier blames to go back to: (commit, file, first line shown).
    pub back: Vec<(Option<String>, String, usize)>,
    /// False while a commit is being looked at in the history (a banner brings it back).
    pub shown: bool,
}

impl FileView {
    pub fn new(mode: Mode) -> Self {
        Self { mode, blame: None, history: None, back: Vec::new(), shown: true }
    }

    /// The file or folder shown, for titles.
    pub fn subject(&self) -> String {
        match (self.mode, &self.blame, &self.history) {
            (Mode::Blame, Some(b), _) => b.path.clone(),
            (_, _, Some(h)) => h.title(),
            (_, Some(b), None) => b.path.clone(),
            _ => String::new(),
        }
    }

    /// The first line on screen in blame (where Back returns to).
    pub fn top_line(&self) -> usize {
        self.blame.as_ref().map(|b| (b.view.0 / DIFF_ROW).floor() as usize).unwrap_or(0)
    }

    fn is_folder(&self) -> bool {
        self.history.as_ref().is_some_and(|h| h.kind == HistoryKind::Folder) && self.blame.is_none()
    }
}

/// Short form of a commit id, or "working copy".
fn rev_label(rev: Option<&str>) -> String {
    match rev {
        None => "working copy".into(),
        Some(r) => {
            let (base, suffix) = r.split_at(r.find(['^', '~']).unwrap_or(r.len()));
            if base.len() >= 40 { format!("{}{suffix}", &base[..8]) } else { r.to_string() }
        }
    }
}

/// "at 1a2b3c4d", or "working copy".
fn at_label(rev: Option<&str>) -> String {
    match rev {
        None => rev_label(None),
        Some(_) => format!("at {}", rev_label(rev)),
    }
}

/// Blame of a file as a commit left it (its parent, if the commit deleted it).
pub fn blame_at(c: &FileCommit) -> FileOpen {
    if c.change == Change::Deleted {
        FileOpen::Blame { rev: Some(format!("{}^", c.hash)), path: c.path.clone(), line: None, back: true }
    } else {
        FileOpen::Blame { rev: Some(c.hash.clone()), path: c.path.clone(), line: None, back: true }
    }
}

/// "Blame the parent" for one line: the same place just before its commit.
fn parent_blame(b: &Blame, line: usize) -> Option<FileOpen> {
    let (c, o) = b.owner(line)?;
    if c.uncommitted() {
        // Uncommitted lines: the file as last committed.
        return Some(FileOpen::Blame { rev: Some("HEAD".into()), path: b.path.clone(), line: Some(line as u32 + 1), back: true });
    }
    let (prev, prev_path) = c.previous.clone()?;
    Some(FileOpen::Blame { rev: Some(prev), path: prev_path, line: Some(o.orig_line), back: true })
}

fn lines_history(b: &Blame, (a, z): (usize, usize)) -> FileOpen {
    FileOpen::Lines { rev: b.rev.clone(), path: b.path.clone(), start: a as u32 + 1, end: z as u32 + 1 }
}

// ---------------------------------------------------------------------------
// Drawing

/// A bar over the history while the file view is put aside, to get back to it.
pub fn banner(ui: &mut Ui, rv: &RepoView, actions: &mut Vec<Action>) {
    let Some(fv) = rv.file_view.as_ref().filter(|f| !f.shown) else { return };
    let what = if fv.mode == Mode::Blame { "Blame" } else { "History" };
    egui::Panel::top("file_view_banner")
        .exact_size(32.0)
        .frame(egui::Frame::new().fill(BLUE.gamma_multiply(0.12)).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, BLUE.gamma_multiply(0.4))))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(format!("{what} of {}", fv.subject())).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        actions.push(Action::CloseFile);
                    }
                    if ui.small_button(format!("Back to {}", what.to_lowercase())).clicked() {
                        actions.push(Action::File(FileOpen::Show));
                    }
                });
            });
        });
}

pub fn show(ui: &mut Ui, rv: &mut RepoView, view: &mut ViewPrefs, actions: &mut Vec<Action>) {
    let keys = !rv.modal && !ui.ctx().text_edit_focused();
    let repo = rv.path.clone();
    let Some(fv) = rv.file_view.as_mut() else { return };
    if keys {
        global_keys(ui, fv, actions);
    }
    egui::Panel::top("file_view_bar")
        .exact_size(44.0)
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 0)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| header(ui, fv, view, actions));
    match fv.mode {
        Mode::Blame => match fv.blame.as_mut() {
            Some(b) => blame_panel(ui, b, keys, actions),
            None => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Folders can't be blamed").color(DIM)));
            }
        },
        Mode::History => {
            if let Some(h) = fv.history.as_mut() {
                history_panel(ui, h, &repo, keys, actions);
            }
        }
    }
}

fn global_keys(ui: &mut Ui, fv: &FileView, actions: &mut Vec<Action>) {
    let (b, h, esc, back) = ui.input_mut(|i| {
        let plain = |i: &egui::InputState, k| i.key_pressed(k) && !i.modifiers.command && !i.modifiers.alt;
        (plain(i, egui::Key::B), plain(i, egui::Key::H), i.key_pressed(egui::Key::Escape), i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft))
    });
    if b && fv.mode != Mode::Blame && !fv.is_folder() {
        actions.push(Action::File(FileOpen::Mode(Mode::Blame)));
    }
    if h && fv.mode != Mode::History {
        actions.push(Action::File(FileOpen::Mode(Mode::History)));
    }
    if back && !fv.back.is_empty() {
        actions.push(Action::File(FileOpen::Back));
    }
    if esc {
        // Esc first drops picked lines, then closes.
        let picked = fv.mode == Mode::Blame && fv.blame.as_ref().is_some_and(|b| b.sel.is_some());
        if !picked {
            actions.push(Action::CloseFile);
        }
    }
}

fn header(ui: &mut Ui, fv: &mut FileView, view: &mut ViewPrefs, actions: &mut Vec<Action>) {
    ui.horizontal_centered(|ui| {
        if !fv.back.is_empty() {
            let r = ui.button("← Back").on_hover_text("Return to the blame you were looking at (Alt+←)");
            if r.clicked() {
                actions.push(Action::File(FileOpen::Back));
            }
        }
        // Blame | History
        let folder = fv.is_folder();
        let blame = ui.add_enabled(!folder, egui::Button::selectable(fv.mode == Mode::Blame, "Blame"));
        if blame.on_hover_text("Who last changed each line (B)").on_disabled_hover_text("Folders can't be blamed").clicked() && fv.mode != Mode::Blame {
            actions.push(Action::File(FileOpen::Mode(Mode::Blame)));
        }
        if ui.add(egui::Button::selectable(fv.mode == Mode::History, "History")).on_hover_text("Commits that changed it (H)").clicked() && fv.mode != Mode::History {
            actions.push(Action::File(FileOpen::Mode(Mode::History)));
        }
        ui.add_space(8.0);
        let (title, sub) = match (fv.mode, &fv.blame, &fv.history) {
            (Mode::Blame, Some(b), _) => (b.path.clone(), at_label(b.rev.as_deref())),
            (_, _, Some(h)) => {
                let sub = match &h.kind {
                    HistoryKind::Lines { rev, .. } => at_label(rev.as_deref()),
                    HistoryKind::Folder => "folder".into(),
                    HistoryKind::File => "following renames".into(),
                };
                (h.title(), sub)
            }
            _ => (String::new(), String::new()),
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("✕ Close").on_hover_text("Back to the history (Esc)").clicked() {
                actions.push(Action::CloseFile);
            }
            if fv.mode == Mode::Blame {
                blame_options(ui, fv.blame.as_ref(), view, actions);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let g = trunc(ui.painter(), &title, theme::mono(13.0), TEXT, (ui.available_width() - 120.0).max(60.0));
                ui.label(g);
                ui.label(RichText::new(sub).small().color(DIM));
            });
        });
    });
}

fn blame_options(ui: &mut Ui, b: Option<&Blame>, view: &mut ViewPrefs, actions: &mut Vec<Action>) {
    let before = view.blame;
    let opts = &mut view.blame;
    if let Some(ig) = b.and_then(|b| b.ignore.as_ref()) {
        ui.checkbox(&mut opts.ignore_revs, "Skip listed commits").on_hover_text(format!(
            "Look past the commits listed in {} (formatting changes and the like){}",
            ig.file,
            if ig.configured { ", as blame.ignoreRevsFile asks" } else { "" }
        ));
    }
    let moves = match opts.moves {
        git::BlameMoves::Off => "Moved lines: off",
        git::BlameMoves::File => "Moved lines: this file",
        git::BlameMoves::Files => "Moved lines: all files",
    };
    let combo = egui::ComboBox::from_id_salt("blame_moves").selected_text(moves).show_ui(ui, |ui| {
        ui.selectable_value(&mut opts.moves, git::BlameMoves::Off, "Off").on_hover_text("Blame whoever last touched the line");
        ui.selectable_value(&mut opts.moves, git::BlameMoves::File, "Moved within this file (-M)").on_hover_text("Look past commits that only moved lines around in the file");
        ui.selectable_value(&mut opts.moves, git::BlameMoves::Files, "Moved or copied from other files (-C)")
            .on_hover_text("Also look past lines moved or copied from other files changed in the same commit (slower)");
    });
    combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, moves));
    ui.checkbox(&mut opts.ignore_whitespace, "Ignore whitespace").on_hover_text("Changes to spaces and indentation don't count (-w)");
    if view.blame != before {
        actions.push(Action::File(FileOpen::Reload));
    }
}

// ---------------------------------------------------------------------------
// Blame

fn age_legend(ui: &mut Ui) {
    ui.label(RichText::new("Newer").small().color(FAINT));
    for k in (0..8).rev() {
        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, theme::age(k as f32 / 7.0));
    }
    ui.label(RichText::new("Older").small().color(FAINT));
}

fn blame_panel(ui: &mut Ui, b: &mut Blame, keys: bool, actions: &mut Vec<Action>) {
    let total = match &b.lines {
        None => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(20.0).color(ACCENT)));
            return;
        }
        Some(Err(e)) => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(format!("Couldn't show this file\n{e}")).color(WARN)));
            return;
        }
        Some(Ok(l)) => l.len(),
    };
    if keys {
        blame_keys(ui, b, total, actions);
    }

    // Progress, then what's picked or the age scale.
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let picked = b.picked();
    let fill = if picked.is_some() { Color32::from_rgb(32, 48, 66) } else { BG };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 3.0))).layout(Layout::left_to_right(Align::Center)));
    let bar = &mut child;
    if let Some(e) = &b.error {
        bar.label(RichText::new(format!("⚠ {e}")).color(WARN)).on_hover_text(e);
    } else if !b.done {
        bar.add(egui::Spinner::new().size(12.0).color(ACCENT));
        bar.label(RichText::new(format!("Blaming… {} of {total} lines", b.blamed())).small().color(DIM));
    } else {
        let n = b.commits.len();
        bar.label(RichText::new(format!("{total} line{} from {n} commit{}", if total == 1 { "" } else { "s" }, if n == 1 { "" } else { "s" })).small().color(DIM));
    }
    bar.with_layout(Layout::right_to_left(Align::Center), |ui| match picked {
        Some((a, z)) => {
            if ui.button("Clear").on_hover_text("Esc").clicked() {
                b.sel = None;
            }
            let n = z - a + 1;
            if ui.button(if n == 1 { "History of this line" } else { "History of these lines" }).on_hover_text("Every commit that changed them (L)").clicked() {
                actions.push(Action::File(lines_history(b, (a, z))));
            }
            if let Some(go) = parent_blame(b, z)
                && ui.button("Blame the parent").on_hover_text("Blame this place as it was before its commit, to look past a formatting or moving change (P)").clicked()
            {
                actions.push(Action::File(go));
            }
            ui.label(RichText::new(if a == z { format!("Line {} picked", a + 1) } else { format!("Lines {}–{} picked", a + 1, z + 1) }).strong().color(TEXT));
        }
        None => age_legend(ui),
    });

    // The lines, with the gutter pinned on the left. (Taken out while drawing, so the
    // rows can update the picks; put back below.)
    let Some(Ok(lines)) = b.lines.take() else { return };
    if b.dragging && !ui.input(|i| i.pointer.primary_down()) {
        b.dragging = false;
    }
    let char_w = ui.fonts_mut(|f| f.glyph_width(&theme::mono(12.5), 'M'));
    let content_w = (GUTTER_W + NUM_W + 16.0 + b.max_cols as f32 * char_w + 20.0).max(ui.available_width());
    let mut sa = egui::ScrollArea::both().auto_shrink(false).id_salt(("blame", &b.path, &b.rev));
    if let Some(r) = b.scroll_to.take() {
        let furthest = (total as f32 * DIFF_ROW - ui.available_height()).max(0.0);
        sa = sa.vertical_scroll_offset((r as f32 * DIFF_ROW).min(furthest));
    }
    let hover_before = b.hover.take();
    let mut menu_line = None;
    let out = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, DIFF_ROW, total, |ui, range| {
            ui.set_min_width(content_w);
            let clip = ui.clip_rect();
            for ri in range {
                let (rect, _) = ui.allocate_exact_size(vec2(content_w, DIFF_ROW), Sense::hover());
                let y = rect.center().y;
                let owner = b.owner(ri).map(|(c, o)| (c.clone(), o));
                let code = Rect::from_x_y_ranges((clip.left() + GUTTER_W + NUM_W)..=rect.right(), rect.y_range());
                let resp = ui.interact(code, ui.scope_id().with(("blame_line", ri)), Sense::click_and_drag());
                name_it(&resp, egui::Role::Row, format!("line {}", ri + 1));
                // Picking lines: click, shift-click, drag.
                let shift = ui.input(|i| i.modifiers.shift);
                if resp.drag_started() {
                    b.sel = Some((ri, ri));
                    b.dragging = true;
                } else if resp.clicked() {
                    b.sel = match b.sel {
                        Some((a, _)) if shift => Some((a, ri)),
                        Some((a, z)) if a == ri && z == ri => None,
                        _ => Some((ri, ri)),
                    };
                }
                if b.dragging
                    && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.y_range().contains(p.y))
                    && let Some((a, _)) = b.sel
                {
                    b.sel = Some((a, ri));
                }
                if resp.secondary_clicked() && !b.picked().is_some_and(|(a, z)| (a..=z).contains(&ri)) {
                    b.sel = Some((ri, ri));
                }
                resp.context_menu(|ui| menu_line = Some((ri, line_menu(ui, b, ri))));
                let p = ui.painter().clone();
                let picked = b.picked().is_some_and(|(a, z)| (a..=z).contains(&ri));
                let same_commit = owner.as_ref().is_some_and(|(_, o)| Some(o.commit) == hover_before);
                if picked {
                    p.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(90, 160, 255, 48));
                } else if same_commit {
                    p.rect_filled(rect, 0.0, Color32::from_white_alpha(8));
                }
                let tx = rect.left() + GUTTER_W + NUM_W + 12.0;
                match b.syntax.as_ref().and_then(|h| h.line(ri as u32 + 1, &lines[ri])) {
                    Some(spans) => {
                        let dark = ui.visuals().dark_mode;
                        let g = p.layout_job(views::code_job(&lines[ri], spans, theme::syntax(highlight::Tok::Plain, dark), dark, views::Emphasis::NONE));
                        p.galley(pos2(tx, y - g.size().y / 2.0), g, CODE);
                    }
                    None => {
                        p.text(pos2(tx, y), Align2::LEFT_CENTER, views::display_line(&lines[ri]), theme::mono(12.5), CODE);
                    }
                }

                // Gutter, drawn last so scrolled code slides under it.
                let g = Rect::from_min_size(pos2(clip.left(), rect.top()), vec2(GUTTER_W, DIFF_ROW));
                let nums = Rect::from_min_size(pos2(g.right(), rect.top()), vec2(NUM_W, DIFF_ROW));
                p.rect_filled(g, 0.0, SIDEBAR);
                p.rect_filled(nums, 0.0, BG);
                if picked {
                    p.rect_filled(Rect::from_min_size(nums.left_top(), vec2(3.0, DIFF_ROW)), 0.0, BLUE);
                }
                p.text(pos2(nums.right() - 8.0, y), Align2::RIGHT_CENTER, (ri + 1).to_string(), theme::mono(11.5), FAINT);
                let Some((c, o)) = owner else { continue };
                let prev = if ri > 0 { b.owners[ri - 1].map(|o| o.commit) } else { None };
                let next = b.owners.get(ri + 1).copied().flatten().map(|o| o.commit);
                let first = prev != Some(o.commit);
                let second = !first && ri >= 1 && (ri < 2 || b.owners[ri - 2].map(|o| o.commit) != Some(o.commit));
                let color = b.age_color(&c);
                let gr = ui.interact(g, ui.scope_id().with(("blame_gutter", ri)), Sense::click());
                let hovered = gr.hovered() || hover_before == Some(o.commit);
                if hovered {
                    p.rect_filled(g, 0.0, HOVER);
                }
                if gr.hovered() {
                    b.hover = Some(o.commit);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // Age: a bar down the edge, stronger for newer changes.
                let mut bar = Rect::from_min_size(g.left_top() + vec2(2.0, 0.0), vec2(4.0, DIFF_ROW));
                if first {
                    bar.min.y += 1.0;
                }
                if next != Some(o.commit) {
                    bar.max.y -= 1.0;
                }
                p.rect_filled(bar, 1.0, color);
                if first && ri > 0 {
                    p.line_segment([g.left_top() + vec2(10.0, 0.0), g.right_top()], Stroke::new(1.0, BORDER));
                }
                let x = g.left() + 14.0;
                if first {
                    let label = if c.uncommitted() { "Not committed yet".to_string() } else { c.author.clone() };
                    p.text(pos2(x, y), Align2::LEFT_CENTER, if c.uncommitted() { "" } else { c.short() }, theme::mono(11.5), DIM);
                    let when = if c.uncommitted() { String::new() } else { theme::relative(c.time) };
                    let wg = p.layout_no_wrap(when, theme::prop(11.5), FAINT);
                    let ww = wg.size().x;
                    p.galley(pos2(g.right() - 8.0 - ww, y - wg.size().y / 2.0), wg, FAINT);
                    let ax = if c.uncommitted() { x } else { x + 70.0 };
                    let a = trunc(&p, &label, theme::prop(12.0), if c.uncommitted() { ACCENT } else { TEXT }, (g.right() - 16.0 - ww - ax).max(10.0));
                    p.galley(pos2(ax, y - a.size().y / 2.0), a, TEXT);
                } else if second && !c.uncommitted() {
                    let s = trunc(&p, &c.summary, theme::prop(11.5), FAINT, g.width() - 24.0);
                    p.galley(pos2(x, y - s.size().y / 2.0), s, FAINT);
                }
                let short = if c.uncommitted() { "uncommitted".to_string() } else { c.short().to_string() };
                name_it(&gr, egui::Role::Button, format!("{short} {}", c.summary));
                let file_note = match b.paths.get(o.path as usize) {
                    Some(p) if *p != b.path => format!("\nline {} of {p} in that commit", o.orig_line),
                    _ => String::new(),
                };
                let boundary = if c.boundary { "\nThe oldest commit blame looked at: these lines may be older." } else { "" };
                let tip = if c.uncommitted() {
                    "Changed in your working copy, not committed yet.\nClick to see your changes.".to_string()
                } else {
                    format!(
                        "{}\n\n{}  ·  {} <{}>\n{} ({}){file_note}{boundary}\n\nClick to show this commit in the history",
                        c.summary,
                        c.short(),
                        c.author,
                        c.email,
                        theme::date_time(c.time),
                        theme::relative(c.time)
                    )
                };
                if gr.on_hover_text(tip).clicked() {
                    actions.push(if c.uncommitted() { Action::ShowWorkingDir } else { Action::ShowCommit(c.hash.clone()) });
                }
            }
        })
    });
    b.lines = Some(Ok(lines));
    let state = out.inner.state;
    b.view = (state.clamped_offset().y, out.inner.inner_rect.height());
    if let Some((_, Some(a))) = menu_line {
        actions.push(a);
    }
}

/// The right-click menu of a blamed line. Returns what to do.
fn line_menu(ui: &mut Ui, b: &Blame, ri: usize) -> Option<Action> {
    let (a, z) = b.picked().unwrap_or((ri, ri));
    let mut out = None;
    let owner = b.owner(ri).map(|(c, _)| c.clone());
    if let Some(c) = &owner {
        if c.uncommitted() {
            if ui.button("Show uncommitted changes").clicked() {
                out = Some(Action::ShowWorkingDir);
            }
        } else if ui.button(format!("Show commit {} in history", c.short())).clicked() {
            out = Some(Action::ShowCommit(c.hash.clone()));
        }
        let parent = parent_blame(b, ri);
        let tip = if c.boundary { "This is the oldest commit blame looked at" } else { "This commit added the file" };
        let label = match &c.previous {
            Some((h, _)) if !c.uncommitted() => format!("Blame the parent ({})", &h[..h.len().min(8)]),
            _ => "Blame the parent".to_string(),
        };
        if ui.add_enabled(parent.is_some(), egui::Button::new(label)).on_disabled_hover_text(tip).clicked() {
            out = parent.map(Action::File);
        }
    }
    let n = z - a + 1;
    if ui.button(if n == 1 { "History of this line".to_string() } else { format!("History of these {n} lines") }).clicked() {
        out = Some(Action::File(lines_history(b, (a, z))));
    }
    if let Some(c) = owner.filter(|c| !c.uncommitted()) {
        ui.separator();
        if ui.button("Copy commit id").clicked() {
            out = Some(Action::Copy(c.hash.clone()));
        }
    }
    out
}

fn nav_key(i: &egui::InputState) -> Option<NavKey> {
    [
        (egui::Key::ArrowUp, NavKey::Up),
        (egui::Key::ArrowDown, NavKey::Down),
        (egui::Key::PageUp, NavKey::PageUp),
        (egui::Key::PageDown, NavKey::PageDown),
        (egui::Key::Home, NavKey::Home),
        (egui::Key::End, NavKey::End),
    ]
    .into_iter()
    .find(|(k, _)| i.key_pressed(*k) && !i.modifiers.alt && !i.modifiers.command)
    .map(|(_, n)| n)
}

fn blame_keys(ui: &mut Ui, b: &mut Blame, total: usize, actions: &mut Vec<Action>) {
    let (nav, shift, enter, esc, parent, lines) = ui.input(|i| {
        let plain = |k| i.key_pressed(k) && !i.modifiers.command && !i.modifiers.alt;
        (nav_key(i), i.modifiers.shift, i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), plain(egui::Key::P), plain(egui::Key::L))
    });
    if esc {
        b.sel = None;
    }
    if let Some(k) = nav {
        let page = ((b.view.1 / DIFF_ROW) as usize).saturating_sub(1).max(1);
        let top = (b.view.0 / DIFF_ROW).ceil() as usize;
        let (anchor, cur) = b.sel.unwrap_or((top, top));
        let to = if b.sel.is_none() { Some(cur.min(total.saturating_sub(1))) } else { views::nav_target(cur, total, k, page) };
        if let Some(t) = to {
            b.sel = Some(if shift { (anchor, t) } else { (t, t) });
            if let Some(off) = views::keep_visible(t, b.view.0, b.view.1, DIFF_ROW) {
                b.scroll_to = Some((off / DIFF_ROW).round() as usize);
            }
        }
    }
    let Some((a, z)) = b.picked() else { return };
    let cur = b.sel.map(|s| s.1).unwrap_or(z);
    if enter && let Some((c, _)) = b.owner(cur) {
        actions.push(if c.uncommitted() { Action::ShowWorkingDir } else { Action::ShowCommit(c.hash.clone()) });
    }
    if parent && let Some(go) = parent_blame(b, cur) {
        actions.push(Action::File(go));
    }
    if lines {
        actions.push(Action::File(lines_history(b, (a, z))));
    }
}

// ---------------------------------------------------------------------------
// History

fn history_panel(ui: &mut Ui, h: &mut History, repo: &std::path::Path, keys: bool, actions: &mut Vec<Action>) {
    let list = match &h.list {
        None => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(20.0).color(ACCENT)));
            return;
        }
        Some(Err(e)) => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(format!("Couldn't read the history\n{e}")).color(WARN)));
            return;
        }
        Some(Ok(l)) if l.is_empty() => {
            let what = match h.kind {
                HistoryKind::Folder => "this folder",
                HistoryKind::Lines { .. } => "these lines",
                HistoryKind::File => "this file",
            };
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(format!("No commits changed {what}")).color(DIM)));
            return;
        }
        Some(Ok(l)) => l,
    };
    let n = list.len();
    if keys {
        let (nav, enter) = ui.input(|i| (nav_key(i), i.key_pressed(egui::Key::Enter)));
        if let Some(k) = nav {
            let page = ((h.list_view.1 / HISTORY_ROW) as usize).saturating_sub(1).max(1);
            let to = match h.selected {
                None => Some(0),
                Some(cur) => views::nav_target(cur, n, k, page),
            };
            if let Some(t) = to {
                actions.push(Action::File(FileOpen::SelectCommit(t)));
                if let Some(off) = views::keep_visible(t, h.list_view.0, h.list_view.1, HISTORY_ROW) {
                    h.list_scroll_to = Some((off / HISTORY_ROW).round() as usize);
                }
            }
        }
        if enter && let Some(c) = h.selected_commit() {
            actions.push(Action::ShowCommit(c.hash.clone()));
        }
    }
    let total_w = ui.available_width();
    egui::Panel::left("file_history_list")
        .resizable(true)
        .default_size((total_w * 0.38).clamp(240.0, 520.0))
        .size_range(200.0..=(total_w * 0.7).max(220.0))
        .frame(egui::Frame::new().fill(BG).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            let mut sa = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("file_history", &h.path));
            if let Some(r) = h.list_scroll_to.take() {
                sa = sa.vertical_scroll_offset(r as f32 * HISTORY_ROW);
            }
            let list = h.commits();
            let out = ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let shown = sa.show_rows(ui, HISTORY_ROW, n + usize::from(n >= HISTORY_LIMIT.min(LINE_HISTORY_LIMIT)), |ui, range| {
                    for i in range {
                        let Some((c, _)) = list.get(i) else {
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), HISTORY_ROW), Sense::hover());
                            ui.painter().text(r.center(), Align2::CENTER_CENTER, format!("Showing the latest {n} commits"), theme::prop(11.5), FAINT);
                            continue;
                        };
                        if let Some(a) = history_row(ui, c, h.selected == Some(i), i) {
                            actions.push(a);
                        }
                    }
                });
                (shown.state.clamped_offset().y, shown.inner_rect.height())
            });
            h.list_view = out.inner;
        });
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
        let Some(c) = h.selected_commit().cloned() else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Pick a commit to see what it changed").color(DIM)));
            return;
        };
        egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Show in history").on_hover_text("Enter").clicked() {
                        actions.push(Action::ShowCommit(c.hash.clone()));
                    }
                    if h.kind != HistoryKind::Folder && ui.button("Blame at this commit").clicked() {
                        actions.push(Action::File(blame_at(&c)));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let g = trunc(ui.painter(), &c.summary, theme::prop(14.0), TEXT, ui.available_width().max(40.0));
                        ui.label(g);
                    });
                });
            });
            ui.label(RichText::new(format!("{}  ·  {} <{}>  ·  {}", &c.hash[..c.hash.len().min(12)], c.author, c.email, theme::date_time(c.time))).small().color(DIM));
        });
        match &h.diff {
            None => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| ui.add(egui::Spinner::new().color(ACCENT)));
            }
            Some(Err(e)) => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(format!("Couldn't read the change\n{e}")).color(WARN)));
            }
            Some(Ok(doc)) => {
                let toggled = views::diff_view(ui, doc, DiffMode::Commit, &mut h.diff_scroll, None, repo, actions);
                if let (Some(click), Some(Ok(doc))) = (toggled, h.diff.as_mut()) {
                    doc.apply(click);
                }
            }
        }
    });
}

fn history_row(ui: &mut Ui, c: &FileCommit, selected: bool, i: usize) -> Option<Action> {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, HISTORY_ROW), Sense::click());
    name_it(&resp, egui::Role::Row, &c.summary);
    let p = ui.painter();
    paint_row_bg(p, rect, selected, resp.hovered());
    let x = rect.left() + 14.0;
    let s = trunc(p, &c.summary, theme::prop(13.0), TEXT, rect.width() - 28.0);
    p.galley(pos2(x, rect.top() + 6.0), s, TEXT);
    let mut meta = format!("{}  ·  {}  ·  {}", c.hash.get(..8).unwrap_or(&c.hash), c.author, theme::relative(c.time));
    match (&c.change, &c.old_path) {
        (_, Some(o)) => meta += &format!("  ·  renamed from {o}"),
        (Change::Added, _) => meta += "  ·  added",
        (Change::Deleted, _) => meta += "  ·  deleted",
        _ => {}
    }
    let color = if c.old_path.is_some() { BLUE } else { DIM };
    let m = trunc(p, &meta, theme::prop(11.5), color, rect.width() - 28.0);
    p.galley(pos2(x, rect.top() + 25.0), m, color);
    let mut out = None;
    if resp.double_clicked() {
        out = Some(Action::ShowCommit(c.hash.clone()));
    } else if resp.clicked() {
        out = Some(Action::File(FileOpen::SelectCommit(i)));
    }
    resp.on_hover_text(format!("{}\n{}\nDouble-click to show it in the history", c.summary, c.hash)).context_menu(|ui| {
        if ui.button("Show in history").clicked() {
            out = Some(Action::ShowCommit(c.hash.clone()));
        }
        if ui.button("Blame at this commit").clicked() {
            out = Some(Action::File(blame_at(c)));
        }
        ui.separator();
        if ui.button("Copy commit id").clicked() {
            out = Some(Action::Copy(c.hash.clone()));
        }
    });
    out
}

// ---------------------------------------------------------------------------
// Find file

/// Every file, then every folder holding files (marked `true`).
pub fn with_folders(files: &[String]) -> Vec<(String, bool)> {
    let mut folders = std::collections::BTreeSet::new();
    for f in files {
        let mut p = f.as_str();
        while let Some((dir, _)) = p.rsplit_once('/') {
            if !folders.insert(dir.to_string()) {
                break;
            }
            p = dir;
        }
    }
    files.iter().map(|f| (f.clone(), false)).chain(folders.into_iter().map(|d| (d, true))).collect()
}

/// Paths containing every word of `query` (any case), best first: the last word in
/// the name itself, then files before folders, then shorter paths.
pub fn find_paths(all: &[(String, bool)], query: &str, limit: usize) -> Vec<(String, bool)> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return all.iter().filter(|(_, folder)| !folder).take(limit).cloned().collect();
    }
    let last = words.last().unwrap();
    let mut hits: Vec<(u8, usize, &(String, bool))> = all
        .iter()
        .filter_map(|e| {
            let low = e.0.to_lowercase();
            if !words.iter().all(|w| low.contains(w.as_str())) {
                return None;
            }
            let name = low.rsplit('/').next().unwrap_or(&low);
            let rank = if name.starts_with(last.as_str()) { 0 } else if name.contains(last.as_str()) { 1 } else { 2 };
            Some((rank * 2 + u8::from(e.1), e.0.len(), e))
        })
        .collect();
    hits.sort_by(|a, b| (a.0, a.1, &a.2.0).cmp(&(b.0, b.1, &b.2.0)));
    hits.into_iter().take(limit).map(|(_, _, e)| e.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_file_matches_names_before_folders() {
        let files: Vec<String> = ["src/app.rs", "src/views.rs", "docs/app-guide.md", "README.md", "src/apps/x.rs"].iter().map(|s| s.to_string()).collect();
        let all = with_folders(&files);
        assert!(all.contains(&("src/apps".into(), true)) && all.contains(&("src".into(), true)));
        assert_eq!(all.iter().filter(|e| e.1).count(), 3);
        let names = |q| find_paths(&all, q, 10).into_iter().map(|e| e.0).collect::<Vec<_>>();
        assert_eq!(names("app"), vec!["src/app.rs", "docs/app-guide.md", "src/apps", "src/apps/x.rs"]);
        assert_eq!(names("SRC view"), vec!["src/views.rs"]);
        assert_eq!(names("nothing"), Vec::<String>::new());
        assert_eq!(find_paths(&all, "", 2).len(), 2);
    }

    fn commit(hash: char, time: i64) -> BlameCommit {
        BlameCommit { hash: hash.to_string().repeat(40), time, summary: format!("c{hash}"), ..Default::default() }
    }

    #[test]
    fn results_fill_in_whatever_order_git_finds_them() {
        let mut b = Blame::new(Some("abc".into()), "f.txt".into(), 1, Some(3));
        b.text_loaded(Ok(vec!["a".into(), "b".into(), "c".into(), "d".into()]), None);
        assert_eq!(b.picked(), Some((2, 2)));
        assert_eq!(b.blamed(), 0);
        let range = |commit, line, count, path: &str| git::BlameRange { commit, orig_line: line + 10, line, count, path: path.into() };
        b.add(vec![commit('a', 100)], vec![range(0, 3, 2, "f.txt")]);
        b.add(vec![commit('b', 900)], vec![range(1, 1, 2, "old.txt"), range(0, 99, 1, "f.txt")]);
        assert_eq!(b.blamed(), 4);
        assert_eq!(b.owner(0).unwrap().0.summary, "cb");
        let (c, o) = b.owner(3).unwrap();
        assert_eq!((c.summary.as_str(), o.orig_line), ("ca", 14));
        assert_eq!(b.paths[b.owner(1).unwrap().1.path as usize], "old.txt");
        // Newer is stronger.
        let (old, new) = (b.age_color(&b.commits[0]), b.age_color(&b.commits[1]));
        assert!(new.r() > old.r());
        b.finished(Err("boom".into()));
        assert!(b.done && b.error.is_some());
    }

    #[test]
    fn blaming_the_parent_goes_to_the_line_before_its_commit() {
        let mut b = Blame::new(None, "g.txt".into(), 1, None);
        b.text_loaded(Ok(vec!["x".into(), "y".into()]), None);
        let mut renamed = commit('b', 2);
        renamed.previous = Some(("a".repeat(40), "f.txt".into()));
        let mut zero = commit('0', 0);
        zero.summary = "Version of g.txt from g.txt".into();
        b.add(vec![renamed, zero], vec![git::BlameRange { commit: 0, orig_line: 7, line: 1, count: 1, path: "g.txt".into() }, git::BlameRange { commit: 1, orig_line: 2, line: 2, count: 1, path: "g.txt".into() }]);
        assert_eq!(parent_blame(&b, 0), Some(FileOpen::Blame { rev: Some("a".repeat(40)), path: "f.txt".into(), line: Some(7), back: true }));
        assert_eq!(parent_blame(&b, 1), Some(FileOpen::Blame { rev: Some("HEAD".into()), path: "g.txt".into(), line: Some(2), back: true }));
        assert_eq!(lines_history(&b, (0, 1)), FileOpen::Lines { rev: None, path: "g.txt".into(), start: 1, end: 2 });
        assert_eq!(rev_label(Some(&format!("{}^", "a".repeat(40)))), "aaaaaaaa^");
        assert_eq!(rev_label(None), "working copy");
    }
}
