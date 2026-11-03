//! The workspace overview: every repository in the workspace at a glance.

use crate::app::{Action, Summary, WorktreeInfo, add_menu};
use crate::git;
use crate::theme::{self, *};
use crate::workspace::Workspace;
use egui::{Align, CornerRadius, Layout, Margin, RichText, Sense, Stroke, Ui};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Height of the Open/Fetch/Pull row at the bottom of a card.
const BUTTONS_H: f32 = 28.0;

pub fn show(
    ui: &mut Ui,
    ws: &Workspace,
    summaries: &HashMap<PathBuf, Result<Summary, String>>,
    busy: &[(u64, PathBuf, String)],
    actions: &mut Vec<Action>,
) {
    egui::Frame::new().inner_margin(Margin::symmetric(28, 22)).show(ui, |ui| {
        // Worktrees whose main repository is here are shown inside its card.
        let top = top_level_repos(&ws.repos, summaries);
        let worktree_count: usize = top
            .iter()
            .filter_map(|r| match summaries.get(*r) {
                Some(Ok(s)) => Some(s.worktrees.len()),
                _ => None,
            })
            .sum();
        // Header
        let mut dirty = 0;
        let mut behind = 0;
        let mut ahead = 0;
        for r in &ws.repos {
            if let Some(Ok(s)) = summaries.get(r) {
                dirty += (s.status.change_count() > 0) as usize;
                behind += (s.status.head.behind > 0) as usize;
                ahead += (s.status.head.ahead > 0) as usize;
            }
        }
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(&ws.name).size(24.0).strong());
                let mut parts = vec![format!("{} repositories", top.len())];
                if worktree_count > 0 {
                    parts.push(format!("{worktree_count} worktree{}", if worktree_count == 1 { "" } else { "s" }));
                }
                if dirty > 0 {
                    parts.push(format!("{dirty} with uncommitted changes"));
                }
                if ahead > 0 {
                    parts.push(format!("{ahead} to push"));
                }
                if behind > 0 {
                    parts.push(format!("{behind} to pull"));
                }
                if dirty == 0 && ahead == 0 && behind == 0 && !ws.repos.is_empty() {
                    parts.push("everything is up to date".into());
                }
                ui.label(RichText::new(parts.join("  ·  ")).color(DIM));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                add_menu(ui, "+  Add", actions);
                if ui.button("⟳  Refresh").on_hover_text("Re-read every repository (Ctrl+R)").clicked() {
                    actions.push(Action::RefreshOverview);
                }
                if ui.button("⬇  Pull all").on_hover_text("Fast-forward every repository that has a remote branch").clicked() {
                    actions.push(Action::PullAll);
                }
                if ui.button("⇅  Fetch all").on_hover_text("Download news from every remote, without changing your files").clicked() {
                    actions.push(Action::FetchAll);
                }
            });
        });
        ui.add_space(18.0);

        if ws.repos.is_empty() {
            empty_state(ui, actions);
            return;
        }

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let gap = 16.0;
            let avail = ui.available_width();
            let cols = ((avail + gap) / (320.0 + gap)).floor().max(1.0) as usize;
            let card_w = (avail - gap * (cols as f32 - 1.0)) / cols as f32;
            for (row_i, chunk) in top.chunks(cols).enumerate() {
                // Cards in a row share the tallest one's height (measured last frame),
                // so their buttons line up along the bottom.
                let key = egui::Id::unique(("overview_row_h", &ws.name, row_i, cols));
                let row_h = ui.data(|d| d.get_temp::<f32>(key)).unwrap_or(0.0);
                let mut tallest: f32 = 0.0;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (i, path) in chunk.iter().enumerate() {
                        let idx = ws.repos.iter().position(|r| r == *path).unwrap_or(row_i * cols + i);
                        let is_busy = busy.iter().any(|b| &b.1 == *path);
                        let natural = card(ui, idx, path, summaries.get(*path), card_w, row_h, is_busy, actions);
                        tallest = tallest.max(natural);
                    }
                });
                if (tallest - row_h).abs() > 0.5 {
                    ui.data_mut(|d| d.insert_temp(key, tallest));
                    ui.ctx().request_repaint();
                }
                ui.add_space(gap);
            }
        });
    });
}

fn empty_state(ui: &mut Ui, actions: &mut Vec<Action>) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("This workspace is empty").size(20.0).strong());
        ui.add_space(6.0);
        ui.label(RichText::new("Add the repositories you work on together. You'll see all of them here at once.").color(DIM));
        ui.add_space(16.0);
        if ui.add(egui::Button::new(RichText::new("Add repository…").strong()).fill(ACCENT_DIM)).clicked() {
            actions.push(Action::PickRepo);
        }
        if ui.button("Add every repository in a folder…").clicked() {
            actions.push(Action::PickFolder);
        }
        if ui.button("Clone from a web address…").clicked() {
            let dest = dirs::home_dir().map(|h| h.display().to_string()).unwrap_or_default();
            actions.push(Action::Dialog(crate::app::Dialog::Clone { url: String::new(), dest }));
        }
    });
}

fn card(
    ui: &mut Ui,
    idx: usize,
    path: &PathBuf,
    summary: Option<&Result<Summary, String>>,
    width: f32,
    row_h: f32,
    busy: bool,
    actions: &mut Vec<Action>,
) -> f32 {
    let name = git::repo_name(path);
    let mut natural = 0.0;
    let color = theme::lane(idx);
    let frame = egui::Frame::new()
        .fill(RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16));
    // The card's own click area is created before its contents, so boxes and buttons
    // inside it get their clicks first.
    let resp = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        ui.style_mut().interaction.selectable_labels = false;
        frame.show(ui, |ui| {
        ui.set_width(width - 32.0);
        ui.push_id(path, |ui| ui.vertical(|ui| {
            // Title row
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
                ui.painter().rect_filled(r, 7.0, color.gamma_multiply(0.25));
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    name.chars().next().unwrap_or('?').to_uppercase().to_string(),
                    theme::prop(16.0),
                    color,
                );
                // Leave room for the "⋯" menu so long names are cut instead of widening the card.
                let name_w = (ui.available_width() - 44.0).max(40.0);
                ui.vertical(|ui| {
                    ui.set_max_width(name_w);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(egui::Label::new(RichText::new(&name).size(16.0).strong()).truncate());
                    ui.add(egui::Label::new(RichText::new(path.display().to_string()).small().color(FAINT)).truncate());
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.menu_button(RichText::new("⋯").color(DIM), |ui| {
                        if ui.button("Open").clicked() {
                            actions.push(Action::OpenRepo(path.clone()));
                            ui.close();
                        }
                        if ui.button("Copy folder path").clicked() {
                            actions.push(Action::Copy(path.display().to_string()));
                            ui.close();
                        }
                        ui.separator();
                        if crate::samples::sample_name(path).is_some()
                            && ui.button("Reset sample project…").on_hover_text("Throw away everything in this sample and start it fresh").clicked()
                        {
                            let p = path.clone();
                            actions.push(Action::confirm(
                                "Reset sample project",
                                format!("Start the sample “{name}” fresh?\n\nEverything in it is replaced by the original sample: its commits, branches, tags and any changes you made."),
                                "Reset",
                                true,
                                Action::git(path, format!("Reset {name}"), move |_| crate::samples::reset_sample(&p).map(|()| String::new())),
                            ));
                            ui.close();
                        }
                        if ui.button("Remove from workspace").clicked() {
                            actions.push(Action::RemoveRepo(path.clone()));
                            ui.close();
                        }
                    });
                });
            });
            ui.add_space(8.0);

            match summary {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(RichText::new("Reading…").color(DIM));
                    });
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("⚠ {e}")).color(WARN));
                }
                Some(Ok(s)) => {
                    let st = &s.status;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        let branch = st.head.branch.clone().unwrap_or_else(|| {
                            st.head.oid.as_deref().map(|o| format!("detached at {}", &o[..7.min(o.len())])).unwrap_or_else(|| "no commits".into())
                        });
                        pill(ui, &format!("⎇ {branch}"), BLUE);
                        if s.linked_worktree {
                            pill(ui, "⧉ worktree", BLUE);
                        }
                        let n = st.change_count();
                        if n > 0 {
                            pill(ui, &format!("● {n} changed"), WARN);
                        } else {
                            pill(ui, "✔ clean", ACCENT);
                        }
                        if st.head.ahead > 0 {
                            pill(ui, &format!("↑{}", st.head.ahead), PURPLE);
                        }
                        if st.head.behind > 0 {
                            pill(ui, &format!("↓{}", st.head.behind), PURPLE);
                        }
                        if !st.conflicts.is_empty() {
                            pill(ui, &format!("{} conflicts", st.conflicts.len()), DANGER);
                        }
                        let (subs, not_set_up) = s.submodules;
                        if not_set_up > 0 {
                            pill(ui, &format!("◫ {not_set_up} of {subs} submodules not set up"), WARN);
                        } else if subs > 0 {
                            pill(ui, &format!("◫ {subs} submodule{}", if subs == 1 { "" } else { "s" }), DIM);
                        }
                    });
                    ui.add_space(6.0);
                    if let Some(c) = &s.last {
                        ui.add(egui::Label::new(RichText::new(&c.summary).color(TEXT)).truncate());
                        ui.label(RichText::new(format!("{} · {}", c.author, theme::relative(c.time))).small().color(DIM));
                    } else {
                        ui.label(RichText::new("No commits yet").color(DIM));
                    }
                    if !s.worktrees.is_empty() {
                        ui.add_space(10.0);
                        ui.label(RichText::new(format!("WORKTREES   {}", s.worktrees.len())).size(11.0).strong().color(DIM));
                        ui.add_space(2.0);
                        // Two and a bit worktrees show at once (the cut-off one hints at more);
                        // the rest scroll, so a busy repository's card stays short.
                        let key = ui.scope_id().with("worktree_h");
                        let one = ui.data(|d| d.get_temp::<f32>(key)).unwrap_or(64.0);
                        egui::ScrollArea::vertical().id_salt("worktrees").max_height(one * 2.5).show(ui, |ui| {
                            for (i, w) in s.worktrees.iter().enumerate() {
                                let top = ui.cursor().top();
                                worktree_box(ui, w, actions);
                                let h = ui.cursor().top() - top;
                                if i == 0 && (h - one).abs() > 0.5 {
                                    ui.data_mut(|d| d.insert_temp(key, h));
                                    ui.ctx().request_repaint();
                                }
                            }
                        });
                    }
                }
            }

            // Pad up to the row's height, then the buttons.
            ui.add_space(10.0);
            let used = ui.min_rect().height();
            natural = used + BUTTONS_H;
            ui.add_space((row_h - natural).max(0.0));
            ui.horizontal(|ui| {
                ui.set_height(BUTTONS_H);
                {
                    if ui.add(egui::Button::new(RichText::new("Open").strong()).fill(ACCENT_DIM)).clicked() {
                        actions.push(Action::OpenRepo(path.clone()));
                    }
                    let has_remote = matches!(summary, Some(Ok(s)) if s.has_remote);
                    if ui.add_enabled(has_remote && !busy, egui::Button::new("Fetch")).clicked() {
                        actions.push(Action::args(path, format!("Fetch {name}"), &["fetch", "--all", "--prune"]));
                    }
                    if ui.add_enabled(has_remote && !busy, egui::Button::new("Pull")).clicked() {
                        actions.push(Action::args(path, format!("Pull {name}"), &["pull", "--ff-only"]));
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                    }
                }
            });
        }));
    })
    });
    let card_name = name.clone();
    resp.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Group, true, format!("Card {card_name}")));
    if resp.response.double_clicked() {
        actions.push(Action::OpenRepo(path.clone()));
    }
    natural
}

/// Repositories that get their own card: everything except worktrees whose main
/// repository is also in the list (those appear inside its card instead).
pub fn top_level_repos<'a>(repos: &'a [PathBuf], summaries: &HashMap<PathBuf, Result<Summary, String>>) -> Vec<&'a PathBuf> {
    let mains: HashSet<&PathBuf> = repos
        .iter()
        .filter_map(|r| match summaries.get(r) {
            Some(Ok(s)) if !s.linked_worktree => Some(&s.canonical),
            _ => None,
        })
        .collect();
    repos
        .iter()
        .filter(|r| !matches!(summaries.get(*r), Some(Ok(s)) if s.main_repo.as_ref().is_some_and(|m| mains.contains(m))))
        .collect()
}

/// A worktree as a small box inside its main repository's card.
fn worktree_box(ui: &mut Ui, w: &WorktreeInfo, actions: &mut Vec<Action>) {
    let folder = git::repo_name(&w.path);
    let label = w.branch.clone().unwrap_or_else(|| format!("{folder} (detached)"));
    // Clickable as a whole (sense registered before the contents, so "Open" still works).
    let resp = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 6))
        .outer_margin(Margin { top: 3, bottom: 3, left: 0, right: 0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("⧉").color(BLUE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(!w.prunable, egui::Button::new(RichText::new("Open").size(12.0))).clicked() {
                        actions.push(Action::OpenWorktree(w.path.clone()));
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&label).color(TEXT)).truncate());
                    });
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                match w.status {
                    _ if w.prunable => pill(ui, "folder missing", WARN),
                    Some((n, ahead, behind)) => {
                        if n > 0 {
                            pill(ui, &format!("● {n} changed"), WARN);
                        } else {
                            pill(ui, "✔ clean", ACCENT);
                        }
                        if ahead > 0 {
                            pill(ui, &format!("↑{ahead}"), PURPLE);
                        }
                        if behind > 0 {
                            pill(ui, &format!("↓{behind}"), PURPLE);
                        }
                    }
                    None => pill(ui, "unreadable", WARN),
                }
                if w.locked {
                    pill(ui, "locked", DIM);
                }
            });
        })
    });
    let r = resp.response;
    let box_name = format!("Worktree {label}");
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, !w.prunable, &box_name));
    if r.hovered() && !w.prunable {
        // Outline the box (inside its outer margin) and show a hand cursor.
        let rect = r.rect.shrink2(egui::vec2(0.0, 3.0));
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, BLUE), egui::StrokeKind::Inside);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let mut tip = w.path.display().to_string();
    if let Some(b) = &w.branch {
        tip += &format!("\nBranch: {b}");
    } else if !w.head.is_empty() {
        tip += &format!("\nNo branch (at commit {})", &w.head[..8.min(w.head.len())]);
    }
    if !w.prunable {
        tip += "\nClick to open this worktree";
    }
    if r.clicked() && !w.prunable {
        actions.push(Action::OpenWorktree(w.path.clone()));
    }
    r.on_hover_text(tip);
}

/// A small rounded tag. In a wrapping row it moves to the next line rather than being
/// squeezed, and text too long for a whole line is cut with "…".
pub fn pill(ui: &mut Ui, text: &str, color: egui::Color32) {
    let font = theme::prop(12.0);
    let text_w = ui.painter().layout_no_wrap(text.to_string(), font, color).size().x;
    let pad = 16.0;
    // In a wrapping row "available width" is the whole line, so measure from where
    // the next tag would actually start.
    let line = ui.max_rect();
    let room = |ui: &Ui| line.right() - ui.cursor().min.x;
    let at_line_start = room(ui) >= line.width() - 1.0;
    if text_w + pad > room(ui) && !at_line_start {
        ui.end_row();
    }
    let max_text = (room(ui) - pad).max(20.0);
    let r = egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.set_max_width(max_text);
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).truncate());
        })
        .response;
    if text_w > max_text {
        r.on_hover_text(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(canonical: &str, main_repo: Option<&str>) -> Result<Summary, String> {
        Ok(Summary {
            status: Default::default(),
            last: None,
            has_remote: false,
            linked_worktree: main_repo.is_some(),
            canonical: canonical.into(),
            main_repo: main_repo.map(PathBuf::from),
            worktrees: vec![],
            submodules: (0, 0),
        })
    }

    /// What a set of input events made the overview ask for.
    #[derive(Debug, Default, PartialEq)]
    struct Opened {
        repo: Vec<PathBuf>,
        worktree: Vec<PathBuf>,
    }

    struct Harness {
        ctx: egui::Context,
        ws: Workspace,
        summaries: HashMap<PathBuf, Result<Summary, String>>,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_worktrees(&["/w/app/.claude/worktrees/feature"])
        }

        fn with_worktrees(paths: &[&str]) -> Self {
            let ctx = egui::Context::default();
            theme::apply(&ctx);
            let mut ws = Workspace::new("Office");
            ws.repos = vec!["/w/app".into()];
            let mut main = summary("/w/app", None).unwrap();
            main.worktrees = paths
                .iter()
                .map(|p| WorktreeInfo {
                    path: p.into(),
                    branch: Some("claude/auto-login-brief-email-650a80".into()),
                    head: "1234567890".into(),
                    prunable: false,
                    locked: false,
                    status: Some((6, 0, 0)),
                })
                .collect();
            let summaries = HashMap::from([(PathBuf::from("/w/app"), Ok(main))]);
            let mut h = Self { ctx, ws, summaries, time: 0.0 };
            // Let the layout settle (cards measure their height on the first frames).
            for _ in 0..3 {
                h.frame(vec![]);
            }
            h
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Opened {
            self.time += 0.02;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut actions = Vec::new();
            let (ws, summaries) = (&self.ws, &self.summaries);
            let mut output = self.ctx.run_ui(input, |ui| show(ui, ws, summaries, &[], &mut actions));
            output.textures_delta.clear(); // no screen to upload to
            let mut out = Opened::default();
            for a in actions {
                match a {
                    Action::OpenRepo(p) => out.repo.push(p),
                    Action::OpenWorktree(p) => out.worktree.push(p),
                    _ => {}
                }
            }
            out
        }

        fn clicks(&mut self, pos: egui::Pos2, count: usize) -> Opened {
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            let mut all = self.frame(vec![egui::Event::PointerMoved(pos)]);
            for _ in 0..count {
                for pressed in [true, false] {
                    let o = self.frame(vec![button(pressed)]);
                    all.repo.extend(o.repo);
                    all.worktree.extend(o.worktree);
                }
            }
            // Move away and wait, so the next clicks don't count as a double-click.
            self.frame(vec![egui::Event::PointerMoved(egui::pos2(990.0, 690.0))]);
            self.time += 1.0;
            all
        }
    }

    #[test]
    fn clicking_a_worktree_box_opens_the_worktree_not_the_main_repository() {
        let mut h = Harness::new();
        let wt = PathBuf::from("/w/app/.claude/worktrees/feature");
        let main = PathBuf::from("/w/app");
        let (mut hits_box, mut hits_card) = (0, 0);
        // Sweep over the first card.
        for y in (140..560).step_by(6) {
            for x in (40..380).step_by(20) {
                let p = egui::pos2(x as f32, y as f32);
                let single = h.clicks(p, 1);
                if !single.worktree.is_empty() {
                    assert_eq!(single.worktree, [wt.clone()]);
                    assert!(single.repo.is_empty(), "a click on the worktree box at {p:?} also opened the main repository");
                    let double = h.clicks(p, 2);
                    assert!(double.repo.is_empty(), "double-clicking the worktree box at {p:?} opened the main repository");
                    hits_box += 1;
                } else if single.repo.is_empty() && h.clicks(p, 2).repo == [main.clone()] {
                    hits_card += 1;
                }
            }
        }
        assert!(hits_box > 10, "the worktree box should be clickable over its whole area ({hits_box} hits)");
        assert!(hits_card > 10, "double-clicking elsewhere on the card should still open the main repository ({hits_card} hits)");
    }

    #[test]
    fn many_worktrees_scroll_inside_a_short_card() {
        let paths: Vec<String> = (0..6).map(|i| format!("/w/app/.claude/worktrees/wt{i}")).collect();
        let mut h = Harness::with_worktrees(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        // Which worktrees a click down the middle of the worktree list reaches.
        let reachable = |h: &mut Harness| {
            let mut seen = Vec::new();
            for y in (100..700).step_by(8) {
                for p in h.clicks(egui::pos2(160.0, y as f32), 1).worktree {
                    if !seen.contains(&p) {
                        seen.push(p);
                    }
                }
            }
            seen
        };
        let first = reachable(&mut h);
        assert!(first.len() >= 2 && first.len() <= 3, "two worktrees (and part of a third) should show at once, got {first:?}");
        assert_eq!(first[..2], [PathBuf::from(&paths[0]), PathBuf::from(&paths[1])]);

        // Scroll the list: the last worktree comes into view.
        let over_list = egui::pos2(160.0, 300.0);
        h.frame(vec![egui::Event::PointerMoved(over_list)]);
        for _ in 0..10 {
            h.frame(vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -200.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            }]);
        }
        for _ in 0..30 {
            h.frame(vec![]);
        }
        let after = reachable(&mut h);
        assert!(after.contains(&PathBuf::from(&paths[5])), "scrolling should reveal the last worktree, got {after:?}");
    }

    #[test]
    fn worktrees_fold_into_their_main_repository() {
        let repos: Vec<PathBuf> = ["/w/app", "/w/app-feature", "/w/lib", "/elsewhere/tool-wt", "/w/loading"].iter().map(PathBuf::from).collect();
        let mut s = HashMap::new();
        s.insert(repos[0].clone(), summary("/w/app", None));
        // A worktree of /w/app, listed through a different (symlinked) path: matched by its resolved main path.
        s.insert(repos[1].clone(), summary("/w/app-feature", Some("/w/app")));
        s.insert(repos[2].clone(), summary("/w/lib", None));
        // A worktree whose main repository isn't in the workspace keeps its own card.
        s.insert(repos[3].clone(), summary("/elsewhere/tool-wt", Some("/elsewhere/tool")));
        // Not read yet: shown until we know better.
        let top: Vec<&str> = top_level_repos(&repos, &s).iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(top, ["/w/app", "/w/lib", "/elsewhere/tool-wt", "/w/loading"]);
    }
}
