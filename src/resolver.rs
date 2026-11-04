//! Merge conflicts: the banner shown while a merge, rebase, cherry-pick or revert
//! is waiting, and the full-width resolver that walks through every conflict in
//! the repository, one at a time.

use crate::app::{Action, OpKind, RepoData, RepoView};
use crate::git::{self, ConflictFile, ConflictKind, FileChange, LineKind, OpType, Operation, Region, Resolution, Segment, Side};
use crate::preview::{self, ConflictPictures};
use crate::theme::{self, *};
use crate::views::display_line;
use egui::{Align, Color32, Key, Layout, Margin, Modifiers, RichText, Stroke, Ui};
use std::collections::HashSet;
use std::path::Path;

/// What was chosen for one conflict region.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Open,
    Ours,
    Theirs,
    OursFirst,
    TheirsFirst,
    Edited(String),
}

/// A choice for a whole file (binary, delete/modify, or deleting an add/add file).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Whole {
    Ours,
    Theirs,
    Keep,
    Delete,
}

/// One conflicted file and the choices made for it so far (kept until saved).
pub struct FileState {
    pub path: String,
    pub kind: ConflictKind,
    /// None until it's been read.
    pub file: Option<Result<ConflictFile, String>>,
    /// A choice for each conflict region, in file order.
    pub picks: Vec<Pick>,
    pub whole: Option<Whole>,
    /// The region being edited by hand, and its text so far.
    pub editing: Option<(usize, String)>,
    /// Unchanged stretches unfolded by hand.
    pub unfolded: HashSet<usize>,
    /// Each side's picture, for image files.
    pub pictures: Option<ConflictPictures>,
}

impl FileState {
    fn new(path: String, kind: ConflictKind) -> Self {
        Self { path, kind, file: None, picks: vec![], whole: None, editing: None, unfolded: HashSet::new(), pictures: None }
    }

    fn set_file(&mut self, file: Result<ConflictFile, String>) {
        self.picks = match &file {
            Ok(f) => vec![Pick::Open; f.regions().count()],
            Err(_) => vec![],
        };
        self.file = Some(file);
        self.whole = None;
        self.editing = None;
        self.unfolded.clear();
    }

    /// Choices were made that aren't saved yet.
    pub fn dirty(&self) -> bool {
        self.whole.is_some() || self.picks.iter().any(|p| *p != Pick::Open) || self.editing.is_some()
    }

    /// A text file resolved region by region.
    pub fn regional(&self) -> bool {
        self.kind.has_regions() && matches!(&self.file, Some(Ok(f)) if !f.binary)
    }

    /// Places to stop when walking through the conflicts: one per region, or one for the file.
    pub fn stops(&self) -> usize {
        if self.regional() { self.picks.len().max(1) } else { 1 }
    }

    pub fn stop_done(&self, i: usize) -> bool {
        if self.whole.is_some() {
            return true;
        }
        self.regional() && self.picks.get(i).is_none_or(|p| *p != Pick::Open)
    }

    pub fn open(&self) -> usize {
        (0..self.stops()).filter(|&i| !self.stop_done(i)).count()
    }

    /// Every conflict has a choice, so it can be saved and marked resolved.
    pub fn ready(&self) -> bool {
        matches!(&self.file, Some(Ok(_))) && self.open() == 0
    }

    /// The file's text with every choice applied (open regions keep their markers).
    pub fn result(&self) -> Option<String> {
        let Some(Ok(f)) = &self.file else { return None };
        let crlf = git::uses_crlf(&f.text);
        let mut out = String::new();
        let mut ri = 0;
        for seg in &f.segments {
            match seg {
                Segment::Same(lines) => out.extend(lines.iter().cloned()),
                Segment::Conflict(r) => {
                    out += &pick_text(r, self.picks.get(ri).unwrap_or(&Pick::Open), crlf);
                    ri += 1;
                }
            }
        }
        Some(out)
    }

    /// What saving this file does.
    pub fn resolution(&self) -> Option<Resolution> {
        let p = self.path.clone();
        match self.whole {
            Some(Whole::Ours) => Some(Resolution::Side(p, Side::Ours)),
            Some(Whole::Theirs) => Some(Resolution::Side(p, Side::Theirs)),
            Some(Whole::Keep) => Some(Resolution::Keep(p)),
            Some(Whole::Delete) => Some(Resolution::Delete(p)),
            None if self.regional() => self.result().map(|t| Resolution::Text(p, t)),
            None => None,
        }
    }
}

/// Every conflicted file in the repository, and where you are among their conflicts.
pub struct Resolver {
    pub files: Vec<FileState>,
    /// The file shown.
    pub current: usize,
    /// The conflict (stop) in that file that has the focus.
    pub focus: usize,
    pub show_base: bool,
    /// Show the whole result instead of the conflicts.
    pub preview: bool,
    /// Bring the focused conflict into view on the next frame.
    pub scroll: bool,
    /// On screen; "Back to changes" hides it but keeps the choices made.
    pub shown: bool,
}

impl Resolver {
    /// Start on `open`, among all of `conflicts`.
    pub fn new(conflicts: &[FileChange], open: &str) -> Self {
        let files: Vec<FileState> = conflicts.iter().map(|c| FileState::new(c.path.clone(), conflict_kind(c))).collect();
        let current = files.iter().position(|f| f.path == open).unwrap_or(0);
        Self { files, current, focus: 0, show_base: false, preview: false, scroll: true, shown: true }
    }

    pub fn file(&self) -> &FileState {
        &self.files[self.current]
    }

    /// Match the list to the repository's conflicts after a reload. Files with
    /// unsaved choices keep them; returns the files to (re)read.
    pub fn sync(&mut self, conflicts: &[FileChange]) -> Vec<(String, ConflictKind)> {
        let cur_path = self.files.get(self.current).map(|f| f.path.clone());
        let mut old: Vec<FileState> = std::mem::take(&mut self.files);
        let mut load = Vec::new();
        for c in conflicts {
            let kind = conflict_kind(c);
            let state = match old.iter().position(|f| f.path == c.path && f.kind == kind) {
                Some(i) => old.swap_remove(i),
                None => FileState::new(c.path.clone(), kind),
            };
            if !state.dirty() {
                load.push((state.path.clone(), kind));
            }
            self.files.push(state);
        }
        match cur_path.and_then(|p| self.files.iter().position(|f| f.path == p)) {
            Some(i) => self.current = i,
            None => {
                // The file shown was resolved: go on to the one that took its place.
                self.current = self.current.min(self.files.len().saturating_sub(1));
                self.focus = 0;
                self.scroll = true;
            }
        }
        load
    }

    /// Files read in the background arrived. Ones with choices made meanwhile are left alone.
    pub fn loaded(&mut self, results: Vec<(String, Result<ConflictFile, String>, Option<ConflictPictures>)>) {
        for (path, file, pictures) in results {
            if let Some(f) = self.files.iter_mut().find(|f| f.path == path && !f.dirty()) {
                f.set_file(file);
                f.pictures = pictures;
            }
        }
        if self.files.get(self.current).is_some_and(|f| f.open() > 0 && f.stop_done(self.focus)) || self.focus >= self.file().stops() {
            self.focus = first_open(self.file());
        }
    }

    /// Every stop in the repository: (file, stop).
    fn stops(&self) -> Vec<(usize, usize)> {
        self.files.iter().enumerate().flat_map(|(fi, f)| (0..f.stops()).map(move |s| (fi, s))).collect()
    }

    /// Which conflict has the focus (1-based) and how many there are.
    pub fn position(&self) -> (usize, usize) {
        let all = self.stops();
        let at = all.iter().position(|&s| s == (self.current, self.focus)).unwrap_or(0);
        (at + 1, all.len())
    }

    /// Conflicts with a choice, and all of them.
    pub fn progress(&self) -> (usize, usize) {
        let total: usize = self.files.iter().map(|f| f.stops()).sum();
        let open: usize = self.files.iter().map(|f| f.open()).sum();
        (total - open, total)
    }

    fn go_to(&mut self, (fi, stop): (usize, usize)) {
        self.current = fi;
        self.focus = stop;
        self.scroll = true;
        self.preview = false;
    }

    /// The previous (-1) or next (+1) conflict, across files.
    pub fn step(&mut self, delta: i32) {
        let all = self.stops();
        let (at, n) = self.position();
        let to = (at as i32 - 1 + delta).clamp(0, n as i32 - 1) as usize;
        if let Some(&s) = all.get(to) {
            self.go_to(s);
        }
    }

    /// On to the next conflict that still needs a choice (wrapping around).
    pub fn advance(&mut self) {
        let all = self.stops();
        let (at, n) = self.position();
        if let Some(&s) = (1..n).map(|k| &all[(at - 1 + k) % n]).find(|&&(fi, st)| !self.files[fi].stop_done(st)) {
            self.go_to(s);
        }
    }

    /// Show this file, at its first conflict that still needs a choice.
    pub fn select(&mut self, fi: usize) {
        if fi < self.files.len() {
            let stop = first_open(&self.files[fi]);
            self.go_to((fi, stop));
        }
    }

    /// Files with choices that aren't saved.
    pub fn unsaved(&self) -> Vec<&str> {
        self.files.iter().filter(|f| f.dirty()).map(|f| f.path.as_str()).collect()
    }

    /// What saving every ready file does.
    pub fn ready(&self) -> Vec<Resolution> {
        self.files.iter().filter(|f| f.ready()).filter_map(|f| f.resolution()).collect()
    }

    /// Choose a side for the focused conflict (the arrow-key shortcuts).
    fn choose(&mut self, side: Side) {
        let focus = self.focus;
        let f = &mut self.files[self.current];
        if f.regional() {
            if let Some(p) = f.picks.get_mut(focus) {
                *p = if side == Side::Ours { Pick::Ours } else { Pick::Theirs };
            } else {
                return;
            }
        } else if matches!(&f.file, Some(Ok(file)) if file.binary) {
            f.whole = Some(if side == Side::Ours { Whole::Ours } else { Whole::Theirs });
        } else {
            return;
        }
        self.advance();
    }
}

fn first_open(f: &FileState) -> usize {
    (0..f.stops()).find(|&i| !f.stop_done(i)).unwrap_or(0)
}

pub fn conflict_kind(c: &FileChange) -> ConflictKind {
    match c.change {
        git::Change::Conflict(k) => k,
        _ => ConflictKind::BothModified,
    }
}

/// What one region becomes for a choice.
pub fn pick_text(r: &Region, pick: &Pick, crlf: bool) -> String {
    match pick {
        Pick::Open => r.original(),
        Pick::Ours => r.ours.concat(),
        Pick::Theirs => r.theirs.concat(),
        Pick::OursFirst => r.ours.concat() + &r.theirs.concat(),
        Pick::TheirsFirst => r.theirs.concat() + &r.ours.concat(),
        Pick::Edited(text) => {
            // Typed text uses "\n"; match the file's line endings and end with one.
            let mut t = text.replace("\r\n", "\n");
            if !t.is_empty() && !t.ends_with('\n') {
                t.push('\n');
            }
            if crlf { t.replace('\n', "\r\n") } else { t }
        }
    }
}

/// Names for the two sides: real branch or commit names while an operation is
/// running. Otherwise (a conflicting stash pop, say) the names git wrote into the
/// markers, or plain "ours" and "theirs".
fn side_names(op: Option<&Operation>, file: Option<&ConflictFile>) -> (String, String) {
    if let Some(op) = op {
        return (op.ours.clone(), op.theirs.clone());
    }
    let (o, t) = file.and_then(|f| f.regions().next()).map(|r| r.labels()).unwrap_or_default();
    let or = |s: String, d: &str| if s.is_empty() { d.to_string() } else { s };
    (or(o, "ours"), or(t, "theirs"))
}

/// "main (ours)", but just "ours" when that's already the name.
fn side_title(name: &str, side: &str) -> String {
    if name == side { name.to_string() } else { format!("{name} ({side})") }
}

/// Shorten a long side name for buttons.
fn short(name: &str) -> String {
    if name.chars().count() > 28 { format!("{}…", name.chars().take(27).collect::<String>()) } else { name.to_string() }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

// ---------------------------------------------------------------------------
// Banner

/// The warning shown before aborting: what goes back where, and what's lost.
pub fn abort_warning(op: &Operation, resolved_files: usize, unsaved: &[&str]) -> String {
    let branch = op.branch.clone().unwrap_or_else(|| "HEAD".into());
    let back = match &op.before {
        Some(b) => format!("{branch} goes back to {b}, as it was before the {}.", op.kind.command()),
        None => format!("{branch} goes back to where it was before the {}.", op.kind.command()),
    };
    let mut lost = Vec::new();
    if op.kind == OpType::Rebase {
        lost.push(format!("commits already replayed onto {} (the originals are untouched)", op.ours));
    }
    match resolved_files {
        0 => lost.push("the changes it made so far".into()),
        1 => lost.push("your resolution of 1 file, and every other change it made".into()),
        n => lost.push(format!("your resolutions of {n} files, and every other change it made")),
    }
    if op.remaining > 0 {
        lost.push(format!("the {} commits still waiting to be applied", op.remaining));
    }
    match unsaved {
        [] => {}
        [one] => lost.push(format!("unsaved choices in {one}")),
        many => lost.push(format!("unsaved choices in {} files", many.len())),
    }
    let list: String = lost.iter().map(|l| format!("\n  • {l}")).collect();
    let verb = match op.kind {
        OpType::Merge => format!("Stop merging {} into {branch}?", op.theirs),
        OpType::Rebase => format!("Stop rebasing {branch} onto {}?", op.ours),
        OpType::CherryPick => format!("Stop cherry-picking {}?", op.commit.as_deref().unwrap_or(&op.theirs)),
        OpType::Revert => format!("Stop reverting {}?", op.commit.as_deref().unwrap_or("this commit")),
    };
    format!("{verb}\n\n{back} You'll lose:{list}\n\nThis can't be undone.")
}

/// The bar under the toolbar while an operation waits: what's happening, and
/// Abort / Skip / Continue.
pub fn banner(ui: &mut Ui, rv: &RepoView, actions: &mut Vec<Action>) {
    let Some(data) = &rv.data else { return };
    let Some(op) = &data.operation else { return };
    let conflicts = data.status.conflicts.len();
    let ready = conflicts == 0;
    let color = if ready { ACCENT } else { DANGER };
    let files = plural(conflicts, "conflicted file", "conflicted files");
    egui::Panel::top("operation_banner")
        .exact_size(38.0)
        .frame(egui::Frame::new().fill(color.gamma_multiply(0.14)).inner_margin(Margin::symmetric(14, 0)).stroke(Stroke::new(1.0, color.gamma_multiply(0.5))))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let state = if ready { "all conflicts resolved".to_string() } else { files.clone() };
                ui.label(RichText::new(if ready { "✓" } else { "⚠" }).color(color).strong());
                ui.label(RichText::new(format!("{} · {state}", op.title())).color(TEXT).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let cont = ui.add_enabled(ready, egui::Button::new(RichText::new("Continue").strong()).fill(ACCENT_DIM));
                    if cont.on_hover_text(format!("Finish the {}", op.kind.command())).on_disabled_hover_text(format!("Resolve {files} first")).clicked() {
                        actions.push(continue_action(&rv.path, op.kind, &rv.commit_msg));
                    }
                    if op.can_skip() && ui.button("Skip").on_hover_text("Leave out the commit that stopped and go on with the rest").clicked() {
                        let kind = op.kind;
                        actions.push(Action::confirm(
                            "Skip this commit",
                            format!("Leave out {} and go on with the rest?\n\nIts changes and any resolutions you made for it are dropped.", op.commit.as_deref().unwrap_or("this commit")),
                            "Skip commit",
                            true,
                            Action::git(&rv.path, format!("Skip in {}", kind.command()), move |p| git::skip_operation(p, kind)),
                        ));
                    }
                    if ui.button("Abort").clicked() {
                        let unsaved = rv.resolver.as_ref().map(|r| r.unsaved()).unwrap_or_default();
                        let kind = op.kind;
                        actions.push(Action::confirm(
                            &format!("Abort {}", kind.command()),
                            abort_warning(op, data.status.staged.len(), &unsaved),
                            &format!("Abort {}", kind.command()),
                            true,
                            Action::git(&rv.path, format!("Abort {}", kind.command()), move |p| git::abort_operation(p, kind)),
                        ));
                    }
                    if !ready
                        && rv.resolver.as_ref().is_none_or(|r| !r.shown)
                        && ui.button("Resolve conflicts").on_hover_text("Work through every conflict, one at a time").clicked()
                        && let Some(c) = data.status.conflicts.first()
                    {
                        actions.push(Action::OpenConflict(c.path.clone()));
                    }
                });
            });
        });
}

/// Continue: a merge takes the commit box's message when one was typed.
fn continue_action(repo: &Path, kind: OpType, message: &str) -> Action {
    let msg = (kind == OpType::Merge && !message.trim().is_empty()).then(|| message.to_string());
    Action::Git {
        repo: repo.to_path_buf(),
        label: format!("Continue {}", kind.command()),
        op: Box::new(move |p| git::continue_operation(p, kind, msg.as_deref())),
        // Clears the commit box once it's done.
        kind: OpKind::Commit,
        record: None,
    }
}

// ---------------------------------------------------------------------------
// Resolver

const OURS_C: Color32 = BLUE;
const THEIRS_C: Color32 = PURPLE;
const BASE_C: Color32 = DIM;
/// Unchanged stretches longer than this are folded to their ends.
const FOLD_OVER: usize = 8;
const CONTEXT: usize = 3;
/// Sides go next to each other from this width up.
const SIDE_BY_SIDE: f32 = 1000.0;

/// Something done in the body that needs the whole resolver afterwards.
enum After {
    /// A choice was made: go on to the next open conflict.
    Advance,
    Focus(usize),
}

/// The full-width resolver for every conflict in the repository.
pub fn show(ui: &mut Ui, rv: &mut RepoView, actions: &mut Vec<Action>) {
    let repo = rv.path.clone();
    let data: Option<&RepoData> = rv.data.as_ref();
    let op = data.and_then(|d| d.operation.as_ref());
    let Some(res) = rv.resolver.as_mut() else { return };
    if res.files.is_empty() {
        return;
    }
    let (ours, theirs) = side_names(op, res.file().file.as_ref().and_then(|f| f.as_ref().ok()));
    let merge_tool = op.and_then(|o| o.merge_tool.clone());

    // Keyboard: Alt+↑/↓ move between conflicts, Alt+←/→ take ours or theirs.
    if res.file().editing.is_none() {
        let key = |ui: &mut Ui, k| ui.input_mut(|i| i.consume_key(Modifiers::ALT, k));
        if key(ui, Key::ArrowDown) {
            res.step(1);
        }
        if key(ui, Key::ArrowUp) {
            res.step(-1);
        }
        if key(ui, Key::ArrowLeft) {
            res.choose(Side::Ours);
        }
        if key(ui, Key::ArrowRight) {
            res.choose(Side::Theirs);
        }
    }

    header(ui, &repo, res, &ours, &theirs, merge_tool.as_deref(), actions);
    file_list(ui, res);
    footer(ui, &repo, res, actions);

    let wide = ui.available_width() >= SIDE_BY_SIDE;
    let cur = res.current;
    let Some(file) = res.files[cur].file.take() else {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(22.0).color(ACCENT)));
        });
        return;
    };
    let mut after = None;
    egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(14, 10))).show(ui, |ui| {
        let (show_base, preview, focus) = (res.show_base, res.preview, res.focus);
        let scroll = std::mem::take(&mut res.scroll);
        let fs = &mut res.files[cur];
        egui::ScrollArea::both().auto_shrink(false).id_salt(("resolver", &fs.path)).show(ui, |ui| {
            // A picture stored as text (SVG): the pictures above its text conflicts.
            if let (Some(p), Ok(f)) = (&fs.pictures, &file)
                && !f.binary
                && !preview
            {
                pictures(ui, &fs.path, p, &ours, &theirs);
            }
            match &file {
            Err(e) => {
                ui.label(RichText::new(format!("Couldn't read {}\n{e}", fs.path)).color(DANGER));
            }
            Ok(_) if fs.whole == Some(Whole::Delete) && fs.kind.has_regions() => {
                after = whole_card(ui, fs, "This file will be deleted.", &[]);
            }
            Ok(f) if f.binary => {
                let (o, t) = (format!("Use {}'s version", short(&ours)), format!("Use {}'s version", short(&theirs)));
                let what = format!(
                    "Binary file · changed on both sides\n{ours}: {} · {theirs}: {} · base: {}",
                    size_text(f.sizes[1]),
                    size_text(f.sizes[2]),
                    size_text(f.sizes[0])
                );
                after = whole_card(ui, fs, &what, &[(o.as_str(), Whole::Ours), (t.as_str(), Whole::Theirs)]);
                if let Some(p) = &fs.pictures {
                    pictures(ui, &fs.path, p, &ours, &theirs);
                }
            }
            Ok(f) if !fs.kind.has_regions() => after = one_sided(ui, fs, f, &ours, &theirs, wide),
            Ok(f) if preview => preview_result(ui, fs, f),
            Ok(f) => after = regions(ui, fs, f, &ours, &theirs, show_base, wide, focus, scroll),
            }
        });
    });
    res.files[cur].file = Some(file);
    match after {
        Some(After::Advance) => res.advance(),
        Some(After::Focus(i)) => res.focus = i,
        None => {}
    }
}

fn header(ui: &mut Ui, repo: &Path, res: &mut Resolver, ours: &str, theirs: &str, merge_tool: Option<&str>, actions: &mut Vec<Action>) {
    egui::Panel::top("resolver_head")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back to changes").on_hover_text("Your choices are kept until you come back").clicked() {
                    actions.push(Action::CloseResolver);
                }
                ui.add_space(8.0);
                let (at, n) = res.position();
                ui.label(RichText::new(format!("Conflict {at} of {n}")).strong().color(TEXT).size(15.0));
                let editing = res.file().editing.is_some();
                if ui.add_enabled(at > 1 && !editing, egui::Button::new("↑ Prev conflict")).on_hover_text("Alt+↑").clicked() {
                    res.step(-1);
                }
                if ui.add_enabled(at < n && !editing, egui::Button::new("↓ Next conflict")).on_hover_text("Alt+↓").clicked() {
                    res.step(1);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let f = res.file();
                    if f.regional() {
                        let has_base = matches!(&f.file, Some(Ok(file)) if file.regions().any(|r| r.base.is_some()));
                        ui.checkbox(&mut res.preview, "Preview result").on_hover_text("The whole file as it will be saved, with your choices");
                        ui.add_enabled(has_base && !res.preview, egui::Checkbox::new(&mut res.show_base, "Show base"))
                            .on_hover_text("Also show the common ancestor of both sides")
                            .on_disabled_hover_text("The common ancestor isn't available here");
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let f = &mut res.files[res.current];
                ui.label(RichText::new(&f.path).strong().color(TEXT));
                let what = match &f.file {
                    Some(Ok(file)) if file.binary => "binary · whole-file choices only".to_string(),
                    Some(Ok(_)) if f.regional() => {
                        let left = match f.picks.len() {
                            0 => "no conflict markers left".into(),
                            n => format!("{} of {n} left", f.open()),
                        };
                        // Both sides adding the same name is worth saying.
                        if f.kind == ConflictKind::BothAdded { format!("{} · {left}", describe_kind(f.kind, ours, theirs)) } else { left }
                    }
                    _ => describe_kind(f.kind, ours, theirs),
                };
                ui.label(RichText::new(what).color(DIM));
                ui.add_space(12.0);
                if f.regional() {
                    for (side, name) in [(Side::Ours, ours), (Side::Theirs, theirs)] {
                        let tip = format!("Replace the file with {name}'s version (git checkout --{})", if side == Side::Ours { "ours" } else { "theirs" });
                        if ui.button(format!("Take all from {}", short(name))).on_hover_text(tip).clicked() {
                            let p = f.path.clone();
                            f.picks.iter_mut().for_each(|p| *p = Pick::Open);
                            actions.push(Action::git(repo, format!("Take {name}'s {}", f.path), move |r| git::take_side(r, &p, side)));
                        }
                    }
                }
                if f.kind == ConflictKind::BothAdded && f.whole.is_none() && ui.button(RichText::new("Delete the file").color(DANGER)).clicked() {
                    f.whole = Some(Whole::Delete);
                }
                let tool = ui.add_enabled(merge_tool.is_some(), egui::Button::new("Open in merge tool"));
                let tool = match merge_tool {
                    Some(t) => tool.on_hover_text(format!("Run git mergetool with {t}, and wait for it to close")),
                    None => tool.on_disabled_hover_text("Set merge.tool in your git config to use an external merge tool"),
                };
                if tool.clicked() {
                    let p = f.path.clone();
                    actions.push(Action::git(repo, format!("Merge tool for {}", f.path), move |r| git::run_merge_tool(r, &p)));
                }
            });
        });
}

/// Every conflicted file, with how far along each one is.
fn file_list(ui: &mut Ui, res: &mut Resolver) {
    let mut pick = None;
    egui::Panel::left("resolver_files")
        .default_size(230.0)
        .size_range(160.0..=420.0)
        .resizable(true)
        .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(Margin::symmetric(8, 8)))
        .show(ui, |ui| {
            ui.label(RichText::new(format!("CONFLICTED FILES · {}", res.files.len())).small().color(DIM));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for (i, f) in res.files.iter().enumerate() {
                    let (badge, color) = match &f.file {
                        None => ("…".to_string(), FAINT),
                        Some(Err(_)) => ("error".into(), DANGER),
                        Some(Ok(_)) if f.ready() => ("✓ ready".into(), ACCENT),
                        Some(Ok(_)) if f.regional() => (format!("{} left", f.open()), DANGER),
                        Some(Ok(_)) => ("choose".into(), WARN),
                    };
                    let (name, dir) = match f.path.rsplit_once('/') {
                        Some((d, n)) => (n, d),
                        None => (f.path.as_str(), ""),
                    };
                    ui.horizontal(|ui| {
                        let r = ui.selectable_label(i == res.current, name).on_hover_text(&f.path);
                        if !dir.is_empty() {
                            ui.label(RichText::new(dir).small().color(FAINT));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(badge).small().color(color)));
                        if r.clicked() {
                            pick = Some(i);
                        }
                    });
                }
            });
        });
    if let Some(i) = pick {
        res.select(i);
    }
}

fn footer(ui: &mut Ui, repo: &Path, res: &mut Resolver, actions: &mut Vec<Action>) {
    egui::Panel::bottom("resolver_foot")
        .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(14, 8)).stroke(Stroke::new(1.0, BORDER)))
        .show(ui, |ui| {
            let (done, total) = res.progress();
            let ready = res.ready();
            let mut note = format!("{done} of {total} conflicts have a choice");
            if !ready.is_empty() {
                note += &format!(" · {} ready to save", plural(ready.len(), "file", "files"));
            }
            let note = RichText::new(note).color(if done == total { ACCENT } else { DIM });
            // On narrow windows the progress gets its own line so the buttons never cover it.
            let narrow = ui.available_width() < 760.0;
            if narrow {
                ui.label(note.clone());
            }
            ui.horizontal(|ui| {
                if !narrow {
                    ui.label(note);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let editing = res.file().editing.is_some();
                    let all = ui.add_enabled(!ready.is_empty() && !editing, egui::Button::new(RichText::new(format!("Save & mark all ready ({})", ready.len())).strong()).fill(ACCENT_DIM));
                    if all.on_hover_text("Save every file whose conflicts all have a choice, and mark them resolved").clicked() {
                        let n = ready.len();
                        actions.push(Action::git(repo, format!("Resolve {}", plural(n, "file", "files")), move |r| git::resolve_files(r, &ready)));
                    }
                    let f = res.file();
                    let open = f.open();
                    let can = !editing && f.resolution().is_some() && (f.regional() || open == 0);
                    let mark = ui.add_enabled(can, egui::Button::new("Save & mark resolved")).on_hover_text("Save this file and mark it resolved");
                    if mark.clicked()
                        && let Some(r) = f.resolution()
                    {
                        let a = Action::git(repo, format!("Resolve {}", f.path), move |p| git::resolve_files(p, &[r]));
                        if open > 0 {
                            actions.push(Action::confirm(
                                "Mark as resolved",
                                format!(
                                    "{} in {} still {} conflict markers.\n\nMark it resolved anyway? The markers are saved into the file as they are.",
                                    plural(open, "conflict", "conflicts"),
                                    f.path,
                                    if open == 1 { "has" } else { "have" }
                                ),
                                "Mark resolved",
                                false,
                                a,
                            ));
                        } else {
                            actions.push(a);
                        }
                    }
                    if f.regional() && f.whole.is_none() {
                        let save = ui.add_enabled(f.dirty() && !editing, egui::Button::new("Save")).on_hover_text("Write this file's choices without marking it resolved");
                        if save.clicked()
                            && let Some(text) = f.result()
                        {
                            let p = f.path.clone();
                            let i = res.current;
                            res.files[i].picks.iter_mut().for_each(|p| *p = Pick::Open);
                            actions.push(Action::git(repo, format!("Save {p}"), move |r| git::save_resolution(r, &p, &text, false)));
                        }
                    }
                });
            });
        });
}

fn describe_kind(kind: ConflictKind, ours: &str, theirs: &str) -> String {
    match kind {
        ConflictKind::BothModified => "changed on both sides".into(),
        ConflictKind::BothAdded => format!("added on both {ours} and {theirs}"),
        ConflictKind::DeletedByUs => format!("{ours} deleted it, {theirs} changed it"),
        ConflictKind::DeletedByThem => format!("{theirs} deleted it, {ours} changed it"),
        ConflictKind::AddedByUs => format!("only {ours} has it"),
        ConflictKind::AddedByThem => format!("only {theirs} has it"),
        ConflictKind::BothDeleted => "deleted or renamed on both sides".into(),
    }
}

fn size_text(n: Option<u64>) -> String {
    match n {
        None => "none".into(),
        Some(b) if b < 1024 => format!("{b} bytes"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        Some(b) => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// Each side's version of a conflicted picture, side by side: ours, theirs and
/// the common ancestor (when there is one). A side that deleted the file says so.
fn pictures(ui: &mut Ui, path: &str, p: &ConflictPictures, ours: &str, theirs: &str) {
    let mut cols: Vec<(String, Option<&preview::ImageSide>)> = vec![(short(ours), p.ours.as_ref()), (short(theirs), p.theirs.as_ref())];
    if let Some(b) = &p.base {
        cols.push(("Common ancestor".into(), Some(b)));
    }
    const GAP: f32 = 20.0;
    let n = cols.len() as f32;
    let w = ((ui.available_width().min(1400.0) - GAP * (n - 1.0)) / n).max(80.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        for (i, (name, side)) in cols.iter().enumerate() {
            ui.vertical(|ui| {
                ui.set_width(w);
                let title = match side {
                    Some(s) if s.error.is_none() => format!("{name}  ·  {} × {}", s.size[0], s.size[1]),
                    _ => name.clone(),
                };
                ui.label(RichText::new(title).color(DIM).size(12.0));
                match side {
                    Some(s) => preview::show(ui, s, w, &format!("conflict-{path}-{i}")),
                    None => {
                        ui.label(RichText::new("Deleted here").italics().color(FAINT));
                    }
                }
            });
        }
    });
    ui.add_space(12.0);
}

/// A box with a description and whole-file choices (or the choice made, with Undo).
fn whole_card(ui: &mut Ui, fs: &mut FileState, what: &str, choices: &[(&str, Whole)]) -> Option<After> {
    let mut after = None;
    egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, if fs.whole.is_some() { ACCENT } else { BLUE })).corner_radius(8).inner_margin(Margin::same(14)).show(ui, |ui| {
        ui.set_max_width(720.0);
        let mut lines = what.lines();
        ui.label(RichText::new(lines.next().unwrap_or("")).strong().color(TEXT).size(15.0));
        for l in lines {
            ui.label(RichText::new(l).color(DIM));
        }
        ui.add_space(8.0);
        match fs.whole {
            Some(w) => {
                let chosen = match choices.iter().find(|c| c.1 == w) {
                    Some(c) => c.0.to_string(),
                    None if w == Whole::Delete => "Delete the file".into(),
                    None => "Keep the file".into(),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("✓ {chosen}")).color(ACCENT));
                    if ui.button("Undo").clicked() {
                        fs.whole = None;
                    }
                });
                ui.label(RichText::new("Applied when you save and mark it resolved.").small().color(FAINT));
            }
            None => {
                ui.horizontal(|ui| {
                    for (i, (label, w)) in choices.iter().enumerate() {
                        let b = if *w == Whole::Delete {
                            egui::Button::new(RichText::new(*label).color(DANGER))
                        } else if i == 0 {
                            egui::Button::new(RichText::new(*label).strong()).fill(ACCENT_DIM)
                        } else {
                            egui::Button::new(*label)
                        };
                        if ui.add(b).clicked() {
                            fs.whole = Some(*w);
                            after = Some(After::Advance);
                        }
                    }
                });
            }
        }
    });
    ui.add_space(10.0);
    after
}

/// One line of code with its line number.
fn code_line(ui: &mut Ui, no: Option<usize>, text: &str, color: Color32, wrap: bool, strike: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let n = no.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
        ui.label(RichText::new(n).font(theme::mono(12.0)).color(FAINT));
        let t = display_line(text.trim_end_matches(['\n', '\r']));
        let mut rt = RichText::new(if t.is_empty() { " ".to_string() } else { t }).font(theme::mono(12.5)).color(color);
        if strike {
            rt = rt.strikethrough();
        }
        let l = egui::Label::new(rt);
        ui.add(if wrap { l.wrap() } else { l.extend() });
    });
}

fn code_lines(ui: &mut Ui, lines: &[String], first: Option<usize>, color: Color32, wrap: bool) {
    tight(ui, |ui| {
        for (i, l) in lines.iter().enumerate() {
            code_line(ui, first.map(|f| f + i), l, color, wrap, false);
        }
    });
}

/// Code lines sit close together, like in an editor (not a row of buttons apart).
fn tight(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().interact_size.y = 18.0;
        add(ui);
    });
}

/// Lines both sides agree on; long stretches fold down to their ends.
fn same_lines(ui: &mut Ui, fs: &mut FileState, key: usize, lines: &[String], first_no: usize, at_start: bool, at_end: bool) {
    if lines.len() <= FOLD_OVER || fs.unfolded.contains(&key) {
        code_lines(ui, lines, Some(first_no), DIM, false);
        return;
    }
    let head = if at_start { 0 } else { CONTEXT };
    let tail = if at_end { 0 } else { CONTEXT };
    code_lines(ui, &lines[..head], Some(first_no), DIM, false);
    let hidden = lines.len() - head - tail;
    let r = ui.add(egui::Label::new(RichText::new(format!("          ⋯ {hidden} unchanged lines")).color(FAINT).font(theme::prop(12.0))).sense(egui::Sense::click()));
    if r.on_hover_text("Click to show them").clicked() {
        fs.unfolded.insert(key);
    }
    code_lines(ui, &lines[lines.len() - tail..], Some(first_no + lines.len() - tail), DIM, false);
}

/// One side of a region: a coloured name bar, then its numbered lines.
fn side_block(ui: &mut Ui, title: &str, color: Color32, lines: &[String], first: Option<usize>, wrap: bool) {
    egui::Frame::new().fill(color.gamma_multiply(0.08)).inner_margin(Margin { left: 6, right: 8, top: 4, bottom: 4 }).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(RichText::new(title).color(color).strong().size(12.0));
        if lines.is_empty() {
            ui.label(RichText::new("(nothing)").italics().color(FAINT));
        } else {
            code_lines(ui, lines, first, TEXT, wrap);
        }
    });
}

fn pick_name(pick: &Pick, ours: &str, theirs: &str) -> String {
    match pick {
        Pick::Open => String::new(),
        Pick::Ours => format!("took {ours}"),
        Pick::Theirs => format!("took {theirs}"),
        Pick::OursFirst => format!("took both, {ours} first"),
        Pick::TheirsFirst => format!("took both, {theirs} first"),
        Pick::Edited(_) => "edited by hand".into(),
    }
}

/// Every region of a text file, with the agreed lines between them. Line
/// numbers are those of the file on disk (markers included).
#[allow(clippy::too_many_arguments)]
fn regions(ui: &mut Ui, fs: &mut FileState, f: &ConflictFile, ours: &str, theirs: &str, show_base: bool, wide: bool, focus: usize, scroll: bool) -> Option<After> {
    let total = f.regions().count();
    let last = f.segments.len().saturating_sub(1);
    let crlf = git::uses_crlf(&f.text);
    let mut after = None;
    let mut ri = 0;
    let mut no = 1;
    for (si, seg) in f.segments.iter().enumerate() {
        match seg {
            Segment::Same(lines) => {
                same_lines(ui, fs, si, lines, no, si == 0, si == last);
                no += lines.len();
            }
            Segment::Conflict(r) => {
                let marker_base = !r.markers[1].is_empty();
                let ours_no = no + 1;
                let base_no = ours_no + r.ours.len() + 1;
                let theirs_no = if marker_base { base_no + r.base.as_ref().map_or(0, |b| b.len()) + 1 } else { ours_no + r.ours.len() + 1 };
                let nums = (ours_no, marker_base.then_some(base_no), theirs_no);
                let a = region(ui, fs, ri, total, r, ours, theirs, crlf, show_base, wide, nums, focus == ri, scroll && focus == ri);
                after = after.or(a);
                no = theirs_no + r.theirs.len() + 1;
                ri += 1;
            }
        }
    }
    if f.segments.is_empty() {
        ui.label(RichText::new("This file is empty.").color(FAINT));
    }
    if total == 0 && !f.segments.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No conflict markers are left in this file. Save & mark resolved when it looks right.").color(ACCENT));
    }
    after
}

#[allow(clippy::too_many_arguments)]
fn region(
    ui: &mut Ui,
    fs: &mut FileState,
    ri: usize,
    total: usize,
    r: &Region,
    ours: &str,
    theirs: &str,
    crlf: bool,
    show_base: bool,
    wide: bool,
    (ours_no, base_no, theirs_no): (usize, Option<usize>, usize),
    focused: bool,
    scroll: bool,
) -> Option<After> {
    let pick = fs.picks.get(ri).cloned().unwrap_or(Pick::Open);
    let editing = fs.editing.as_ref().is_some_and(|(i, _)| *i == ri);
    let mut after = None;
    let border = if editing || focused { BLUE } else if pick == Pick::Open { DANGER } else { ACCENT };
    ui.add_space(4.0);
    let frame = egui::Frame::new()
        .stroke(Stroke::new(if focused { 2.0 } else { 1.0 }, border.gamma_multiply(if focused { 1.0 } else { 0.7 })))
        .corner_radius(6)
        .inner_margin(Margin::same(6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}Conflict {} of {total}", if focused { "▸ " } else { "" }, ri + 1)).strong().color(TEXT));
                if editing {
                    ui.label(RichText::new("· editing").color(BLUE));
                } else if pick != Pick::Open {
                    ui.label(RichText::new(format!("✓ {}", pick_name(&pick, ours, theirs))).color(ACCENT));
                } else {
                    ui.label(RichText::new("unresolved").color(DANGER));
                }
            });
            ui.add_space(2.0);
            if editing {
                let mut done = false;
                let mut cancel = false;
                if let Some((_, text)) = fs.editing.as_mut() {
                    ui.add(egui::TextEdit::multiline(text).font(theme::mono(12.5)).desired_width(f32::INFINITY).desired_rows(4).id_salt(("resolve_edit", ri)));
                    ui.horizontal(|ui| {
                        done = ui.add(egui::Button::new(RichText::new("Done").strong()).fill(ACCENT_DIM)).clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                }
                if done {
                    if let Some((_, text)) = fs.editing.take() {
                        fs.picks[ri] = Pick::Edited(text);
                        after = Some(After::Advance);
                    }
                } else if cancel {
                    fs.editing = None;
                }
                return;
            }
            if pick != Pick::Open {
                let text = pick_text(r, &pick, crlf);
                let lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
                side_block(ui, "Result", ACCENT, &lines, None, false);
                ui.horizontal(|ui| {
                    if ui.button("Undo").clicked() {
                        fs.picks[ri] = Pick::Open;
                        after = Some(After::Focus(ri));
                    }
                    if ui.button("Edit").clicked() {
                        fs.editing = Some((ri, text.replace("\r\n", "\n")));
                        after = Some(After::Focus(ri));
                    }
                });
                return;
            }
            let base = if show_base { r.base.as_ref() } else { None };
            let ours_title = side_title(ours, "ours");
            let theirs_title = side_title(theirs, "theirs");
            if wide {
                let n = if base.is_some() { 3 } else { 2 };
                ui.columns(n, |c| {
                    side_block(&mut c[0], &ours_title, OURS_C, &r.ours, Some(ours_no), true);
                    if let Some(b) = base {
                        side_block(&mut c[1], "base (common ancestor)", BASE_C, b, base_no, true);
                    }
                    side_block(&mut c[n - 1], &theirs_title, THEIRS_C, &r.theirs, Some(theirs_no), true);
                });
            } else {
                side_block(ui, &ours_title, OURS_C, &r.ours, Some(ours_no), false);
                if let Some(b) = base {
                    side_block(ui, "base (common ancestor)", BASE_C, b, base_no, false);
                }
                side_block(ui, &theirs_title, THEIRS_C, &r.theirs, Some(theirs_no), false);
            }
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                let (o, t) = (short(ours), short(theirs));
                let choices = [
                    (format!("Take {o}"), Pick::Ours, "Alt+←"),
                    (format!("Take {t}"), Pick::Theirs, "Alt+→"),
                    (format!("Both, {o} first"), Pick::OursFirst, ""),
                    (format!("Both, {t} first"), Pick::TheirsFirst, ""),
                ];
                for (label, p, key) in choices {
                    let b = ui.button(label);
                    let b = if key.is_empty() { b } else { b.on_hover_text(key) };
                    if b.clicked() {
                        fs.picks[ri] = p;
                        after = Some(After::Advance);
                    }
                }
                if ui.button("Edit").on_hover_text("Write the result yourself, starting from both sides").clicked() {
                    fs.editing = Some((ri, pick_text(r, &Pick::OursFirst, false)));
                    after = Some(After::Focus(ri));
                }
            });
        });
    if scroll {
        frame.response.scroll_to_me(Some(Align::Center));
    }
    ui.add_space(4.0);
    after
}

/// The whole file as it will be saved: chosen text in green, unresolved regions in red.
fn preview_result(ui: &mut Ui, fs: &FileState, f: &ConflictFile) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let crlf = git::uses_crlf(&f.text);
    let mut no = 1;
    let mut ri = 0;
    for seg in &f.segments {
        let (lines, fill): (Vec<String>, Option<Color32>) = match seg {
            Segment::Same(lines) => (lines.clone(), None),
            Segment::Conflict(r) => {
                let pick = fs.picks.get(ri).unwrap_or(&Pick::Open);
                ri += 1;
                let text = pick_text(r, pick, crlf);
                (text.split_inclusive('\n').map(String::from).collect(), Some(if *pick == Pick::Open { DEL_BG } else { ADD_BG }))
            }
        };
        let draw = |ui: &mut Ui| code_lines(ui, &lines, Some(no), if fill.is_some() { TEXT } else { DIM }, false);
        match fill {
            Some(c) => {
                egui::Frame::new().fill(c).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    draw(ui);
                });
            }
            None => draw(ui),
        }
        no += lines.len();
    }
}

/// Delete/modify and one-sided conflicts: what each side did, then keep or delete.
fn one_sided(ui: &mut Ui, fs: &mut FileState, f: &ConflictFile, ours: &str, theirs: &str, wide: bool) -> Option<After> {
    let sided = f.sided.as_ref();
    let stage = sided.map_or(2, |s| s.stage);
    let (keeper, deleter) = if stage == 3 { (theirs, ours) } else { (ours, theirs) };
    let kept = if stage == 1 { "the original version".to_string() } else { format!("{keeper}'s version") };
    let what = format!(
        "{}\nSizes: base {} · {ours} {} · {theirs} {}",
        describe_kind(fs.kind, ours, theirs),
        size_text(f.sizes[0]),
        size_text(f.sizes[1]),
        size_text(f.sizes[2])
    );
    let keep = format!("Keep the file ({kept})");
    let after = whole_card(ui, fs, &what, &[(keep.as_str(), Whole::Keep), ("Delete the file", Whole::Delete)]);
    let Some(s) = sided else { return after };

    // What each side did to it, so the choice isn't made blind.
    let deleted_title = format!("{deleter} deleted the file");
    let changed_title = if s.base.is_empty() { format!("{keeper} added the file") } else { format!("{keeper}'s changes since the common ancestor") };
    let deleted = |ui: &mut Ui, wrap: bool| {
        egui::Frame::new().fill(DANGER.gamma_multiply(0.06)).inner_margin(Margin { left: 6, right: 8, top: 4, bottom: 4 }).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(&deleted_title).color(DEL_FG).strong().size(12.0));
            if s.base.is_empty() {
                ui.label(RichText::new("(there was nothing before)").italics().color(FAINT));
            }
            tight(ui, |ui| {
                for (i, l) in s.base.iter().enumerate() {
                    code_line(ui, Some(i + 1), l, DEL_FG, wrap, true);
                }
            });
        });
    };
    let changed = |ui: &mut Ui, wrap: bool| {
        egui::Frame::new().fill(BLUE.gamma_multiply(0.06)).inner_margin(Margin { left: 6, right: 8, top: 4, bottom: 4 }).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(&changed_title).color(BLUE).strong().size(12.0));
            let (added, removed) = s.changes.iter().fold((0, 0), |(a, d), l| match l.kind {
                LineKind::Add => (a + 1, d),
                LineKind::Del => (a, d + 1),
                _ => (a, d),
            });
            ui.label(RichText::new(format!("+{added} −{removed}")).small().color(DIM));
            tight(ui, |ui| for l in &s.changes {
                let (fill, color, sign) = match l.kind {
                    LineKind::Add => (Some(ADD_BG), ADD_FG, "+ "),
                    LineKind::Del => (Some(DEL_BG), DEL_FG, "− "),
                    _ => (None, DIM, "  "),
                };
                let no = l.new_no.or(l.old_no).map(|n| n as usize);
                let text = format!("{sign}{}", l.text);
                match fill {
                    Some(c) => {
                        egui::Frame::new().fill(c).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            code_line(ui, no, &text, color, wrap, false);
                        });
                    }
                    None => code_line(ui, no, &text, color, wrap, false),
                }
            });
        });
    };
    if fs.kind == ConflictKind::BothDeleted {
        deleted(ui, false);
    } else if wide {
        // Ours on the left, theirs on the right, whichever side deleted it.
        ui.columns(2, |c| {
            let (l, r) = if stage == 3 { (0, 1) } else { (1, 0) };
            deleted(&mut c[l], true);
            changed(&mut c[r], true);
        });
    } else {
        changed(ui, false);
        ui.add_space(6.0);
        deleted(ui, false);
    }
    after
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> ConflictFile {
        ConflictFile {
            path: "f".into(),
            kind: ConflictKind::BothModified,
            binary: false,
            text: text.into(),
            segments: git::parse_conflicts(text, 7),
            index_base: false,
            sizes: [None; 3],
            sided: None,
        }
    }

    fn change(path: &str, kind: ConflictKind) -> FileChange {
        FileChange { path: path.into(), old_path: None, change: git::Change::Conflict(kind), submodule: false }
    }

    const TWO: &str = "a\n<<<<<<< HEAD\no\n=======\nt\n>>>>>>> x\nb\n<<<<<<< HEAD\no2\n=======\nt2\n>>>>>>> x\n";

    #[test]
    fn choices_rebuild_the_file() {
        let mut r = FileState::new("f".into(), ConflictKind::BothModified);
        r.set_file(Ok(file(TWO)));
        assert_eq!(r.open(), 2);
        assert_eq!(r.result().unwrap(), TWO, "nothing chosen leaves the file as it was");
        r.picks = vec![Pick::Ours, Pick::TheirsFirst];
        assert_eq!(r.result().unwrap(), "a\no\nb\nt2\no2\n");
        r.picks = vec![Pick::Theirs, Pick::Edited("mine".into())];
        assert_eq!(r.result().unwrap(), "a\nt\nb\nmine\n");
        assert!(r.ready() && r.dirty());
        assert_eq!(r.resolution(), Some(Resolution::Text("f".into(), "a\nt\nb\nmine\n".into())));
        r.picks = vec![Pick::OursFirst, Pick::Edited(String::new())];
        assert_eq!(r.result().unwrap(), "a\no\nt\nb\n");
    }

    #[test]
    fn edits_follow_the_files_line_endings() {
        let text = "<<<<<<< HEAD\r\no\r\n=======\r\nt\r\n>>>>>>> x\r\nend\r\n";
        let mut r = FileState::new("f".into(), ConflictKind::BothModified);
        r.set_file(Ok(file(text)));
        r.picks = vec![Pick::Edited("one\ntwo".into())];
        assert_eq!(r.result().unwrap(), "one\r\ntwo\r\nend\r\n");
    }

    /// a.txt with two regions, then a delete/modify file and a binary file.
    fn three_files() -> Resolver {
        let list = [change("a.txt", ConflictKind::BothModified), change("gone.txt", ConflictKind::DeletedByThem), change("pic.bin", ConflictKind::BothModified)];
        let mut res = Resolver::new(&list, "a.txt");
        let mut bin = file("");
        bin.binary = true;
        let mut gone = file("x\n");
        gone.kind = ConflictKind::DeletedByThem;
        res.loaded(vec![("a.txt".into(), Ok(file(TWO)), None), ("gone.txt".into(), Ok(gone), None), ("pic.bin".into(), Ok(bin), None)]);
        res
    }

    #[test]
    fn walking_through_every_conflict_in_the_repository() {
        let mut res = three_files();
        assert_eq!(res.position(), (1, 4));
        assert_eq!(res.progress(), (0, 4));
        res.step(1);
        assert_eq!((res.current, res.focus), (0, 1));
        res.step(1);
        assert_eq!((res.current, res.position()), (1, (3, 4)), "on into the next file");
        res.step(1);
        res.step(1);
        assert_eq!(res.position(), (4, 4), "stops at the last one");
        res.step(-3);
        assert_eq!(res.position(), (1, 4));

        // A choice moves on to the next conflict that still needs one, across files.
        res.choose(Side::Theirs);
        assert_eq!(res.position(), (2, 4));
        res.choose(Side::Ours);
        assert_eq!((res.current, res.focus), (1, 0));
        // Arrow keys don't decide delete/modify files; binary files take a side.
        res.choose(Side::Ours);
        assert_eq!(res.current, 1);
        res.files[1].whole = Some(Whole::Keep);
        res.advance();
        assert_eq!(res.current, 2);
        res.choose(Side::Theirs);
        assert_eq!(res.progress(), (4, 4));
        assert_eq!(
            res.ready(),
            [
                Resolution::Text("a.txt".into(), "a\nt\nb\no2\n".into()),
                Resolution::Keep("gone.txt".into()),
                Resolution::Side("pic.bin".into(), Side::Theirs),
            ]
        );
        assert_eq!(res.unsaved(), ["a.txt", "gone.txt", "pic.bin"]);
        // Picking a file in the list goes to its first open conflict.
        res.files[0].picks[1] = Pick::Open;
        res.select(0);
        assert_eq!((res.current, res.focus), (0, 1));
    }

    #[test]
    fn reloads_keep_unsaved_choices_and_drop_resolved_files() {
        let mut res = three_files();
        res.files[0].picks[0] = Pick::Ours;
        res.select(1);
        // gone.txt was resolved elsewhere; a.txt still has choices, pic.bin is read again.
        let now = [change("a.txt", ConflictKind::BothModified), change("pic.bin", ConflictKind::BothModified)];
        let load = res.sync(&now);
        assert_eq!(load, [("pic.bin".to_string(), ConflictKind::BothModified)]);
        assert_eq!(res.files.len(), 2);
        assert_eq!(res.current, 1, "on to the file that took its place");
        assert_eq!(res.files[0].picks[0], Pick::Ours);
    }

    #[test]
    fn names_and_descriptions() {
        // Without an operation, the names come from the markers.
        let stash = file("<<<<<<< Updated upstream\no\n=======\nt\n>>>>>>> Stashed changes\n");
        assert_eq!(side_names(None, Some(&stash)), ("Updated upstream".into(), "Stashed changes".into()));
        assert_eq!(side_names(None, Some(&file("<<<<<<<\no\n=======\nt\n>>>>>>>\n"))), ("ours".into(), "theirs".into()));
        assert_eq!(side_names(None, None), ("ours".into(), "theirs".into()));
        assert_eq!((side_title("ours", "ours"), side_title("main", "ours")), ("ours".into(), "main (ours)".into()));
        let kinds = [
            (ConflictKind::BothModified, "changed on both sides"),
            (ConflictKind::BothAdded, "added on both o and t"),
            (ConflictKind::DeletedByUs, "o deleted it, t changed it"),
            (ConflictKind::DeletedByThem, "t deleted it, o changed it"),
            (ConflictKind::AddedByUs, "only o has it"),
            (ConflictKind::AddedByThem, "only t has it"),
            (ConflictKind::BothDeleted, "deleted or renamed on both sides"),
        ];
        for (k, text) in kinds {
            assert_eq!(describe_kind(k, "o", "t"), text);
        }
        assert_eq!([None, Some(12), Some(2048), Some(3 << 20)].map(size_text), ["none", "12 bytes", "2.0 KB", "3.0 MB"]);
        assert_eq!(pick_name(&Pick::TheirsFirst, "o", "t"), "took both, t first");
        assert_eq!(pick_name(&Pick::Open, "o", "t"), "");
        let revert = Operation { kind: OpType::Revert, branch: None, ours: "HEAD".into(), theirs: "before abc".into(), commit: None, step: None, remaining: 0, before: None, merge_tool: None };
        assert!(abort_warning(&revert, 0, &[]).starts_with("Stop reverting this commit?\n\nHEAD goes back to where it was before the revert."));
        // A file that couldn't be read has nothing to choose, and nothing to save.
        let mut broken = FileState::new("x".into(), ConflictKind::BothModified);
        broken.set_file(Err("unreadable".into()));
        assert_eq!((broken.stops(), broken.ready(), broken.resolution()), (1, false, None));
        broken.whole = Some(Whole::Ours);
        assert_eq!(broken.resolution(), Some(Resolution::Side("x".into(), Side::Ours)));
    }

    #[test]
    fn abort_warnings_say_what_is_lost() {
        let op = Operation {
            kind: OpType::Rebase,
            branch: Some("feature/search".into()),
            ours: "main".into(),
            theirs: "a3e4377 “Ignore empty”".into(),
            commit: Some("a3e4377 “Ignore empty”".into()),
            step: Some((2, 5)),
            remaining: 0,
            before: Some("c1d4809".into()),
            merge_tool: None,
        };
        let w = abort_warning(&op, 2, &["src/cart.js"]);
        assert!(w.starts_with("Stop rebasing feature/search onto main?"), "{w}");
        assert!(w.contains("feature/search goes back to c1d4809"));
        assert!(w.contains("commits already replayed onto main"));
        assert!(w.contains("your resolutions of 2 files"));
        assert!(w.contains("unsaved choices in src/cart.js"));
        assert!(abort_warning(&op, 0, &["a", "b"]).contains("unsaved choices in 2 files"));
        let merge = Operation { kind: OpType::Merge, branch: Some("main".into()), ours: "main".into(), theirs: "feature/x".into(), commit: None, step: None, remaining: 0, before: None, merge_tool: None };
        let w = abort_warning(&merge, 0, &[]);
        assert!(w.starts_with("Stop merging feature/x into main?") && w.contains("the changes it made so far"), "{w}");
        let pick = Operation { kind: OpType::CherryPick, remaining: 2, commit: Some("abc “x”".into()), ..merge };
        assert!(abort_warning(&pick, 1, &[]).contains("the 2 commits still waiting"));
    }
}
