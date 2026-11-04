//! Click-through tests: the real app, driven the way a person would use it —
//! clicking buttons and rows found by their names, typing, pressing keys — then
//! checking both what's on screen and what actually happened in the repositories.
//! Every test works in its own temporary folder with its own settings file.

use super::HeadwayApp;
use crate::git;
use crate::workspace::{Config, Workspace};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::{Path, PathBuf};

/// A scratch folder for one test, deleted afterwards.
struct Scratch {
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch(name: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("headway-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Scratch { root }
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    git::run(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
}

/// A repository with a test identity.
fn new_repo(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "Ana Lima"],
        &["config", "user.email", "ana@example.com"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git_ok(&dir, args);
    }
    dir
}

fn write(dir: &Path, file: &str, text: &str) {
    let p = dir.join(file);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, text).unwrap();
}

fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
    write(dir, file, text);
    git_ok(dir, &["add", "-A"]);
    git_ok(dir, &["commit", "-q", "-m", msg]);
}

fn subjects(dir: &Path, rev: &str) -> Vec<String> {
    git_ok(dir, &["log", "--format=%s", rev]).lines().map(String::from).collect()
}

/// The app with a workspace of `repos`; `open` opens the first one as a tab.
fn app(s: &Scratch, repos: &[&Path], open: bool) -> Harness<'static, HeadwayApp> {
    app_sized(s, repos, open, egui::vec2(1600.0, 1000.0))
}

/// Like [`app`], in a window of this size.
fn app_sized(s: &Scratch, repos: &[&Path], open: bool, size: egui::Vec2) -> Harness<'static, HeadwayApp> {
    let mut ws = Workspace::new("Office");
    ws.repos = repos.iter().map(|p| p.to_path_buf()).collect();
    if open {
        ws.open_tabs = vec![ws.repos[0].clone()];
        ws.active_tab = Some(0);
    }
    let cfg = Config::for_test(vec![ws], s.root.join("settings.json"));
    let mut h = Harness::builder()
        .with_size(size)
        // Frames at a normal 60 per second (double-clicks need two clicks within 0.3 s).
        .with_step_dt(1.0 / 60.0)
        // Same fallback fonts as the real window, for symbols the built-in font lacks.
        .with_context_setup(|ctx| ctx.add_font_provider(std::sync::Arc::new(egui_system_font_provider::SystemFontProvider::new())))
        .build_eframe(move |cc| HeadwayApp::with_config(cc.egui_ctx.clone(), cfg, false, None));
    settle(&mut h);
    h
}

/// Run frames until background work is done and the screen has caught up.
fn settle(h: &mut Harness<'static, HeadwayApp>) {
    let mut quiet = 0;
    for _ in 0..3000 {
        h.step();
        if h.state().idle() {
            quiet += 1;
            if quiet >= 3 {
                return;
            }
        } else {
            quiet = 0;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    panic!("the app kept working for too long");
}

/// Click the (first) control with exactly this name, then let the app react.
fn click(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no “{label}” on screen")).click();
    settle(h);
}

/// Right-click a row of the history (not the same text in the details pane).
fn right_click_row(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label)
        .find(|n| n.accesskit_node().role() == egui::Role::Row)
        .unwrap_or_else(|| panic!("no history row “{label}”"))
        .click_secondary();
    settle(h);
}

fn right_click(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no “{label}” on screen")).click_secondary();
    settle(h);
}

fn double_click(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    // Half a second after any earlier click, or egui counts this as a triple-click.
    for _ in 0..30 {
        h.step();
    }
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no “{label}” on screen")).click();
    h.step();
    h.get_all_by_label(label).next().unwrap().click();
    settle(h);
}

fn shows(h: &Harness<'static, HeadwayApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn shows_containing(h: &Harness<'static, HeadwayApp>, part: &str) -> bool {
    h.query_all_by_label_contains(part).next().is_some()
}

/// The text box whose hint text contains `hint`.
fn text_box<'a>(h: &'a Harness<'static, HeadwayApp>, hint: &str) -> egui_kittest::Node<'a> {
    use egui_kittest::kittest::NodeT;
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().placeholder().is_some_and(|p| p.contains(hint)))
        .unwrap_or_else(|| panic!("no text box with hint “{hint}”"))
}

/// Click into the text box that currently contains `part`.
fn click_text_containing(h: &mut Harness<'static, HeadwayApp>, part: &str) {
    h.root()
        .children_recursive()
        .find(|n| {
            matches!(n.accesskit_node().role(), egui::Role::TextInput | egui::Role::MultilineTextInput)
                && n.accesskit_node().value().is_some_and(|v| v.contains(part))
        })
        .unwrap_or_else(|| panic!("no text box containing “{part}”"))
        .click();
    settle(h);
}

/// Click into a text box (found by its hint) and type.
fn type_into(h: &mut Harness<'static, HeadwayApp>, hint: &str, text: &str) {
    text_box(h, hint).click();
    h.step();
    text_box(h, hint).type_text(text);
    settle(h);
}

/// Type into whatever has the keyboard focus (dialogs focus their first field).
fn type_text(h: &mut Harness<'static, HeadwayApp>, text: &str) {
    h.event(egui::Event::Text(text.to_string()));
    settle(h);
}

fn key(h: &mut Harness<'static, HeadwayApp>, k: egui::Key) {
    h.key_press(k);
    settle(h);
}

fn ctrl(h: &mut Harness<'static, HeadwayApp>, k: egui::Key) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, k);
    settle(h);
}

fn hover(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no “{label}” on screen")).hover();
    settle(h);
}

/// The commit currently checked out, by subject.
fn head_subject(dir: &Path) -> String {
    git_ok(dir, &["log", "-1", "--format=%s"]).trim().to_string()
}

fn branch(dir: &Path) -> String {
    git_ok(dir, &["branch", "--show-current"]).trim().to_string()
}

/// A bare "server" repository and a clone of it with a few commits pushed.
fn with_server(s: &Scratch) -> (PathBuf, PathBuf) {
    let server = s.root.join("server.git");
    git_ok(&s.root, &["init", "-q", "--bare", "-b", "main", server.to_str().unwrap()]);
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\n", "Start shop");
    commit(&r, "b.txt", "two\n", "Add cart page");
    git_ok(&r, &["remote", "add", "origin", server.to_str().unwrap()]);
    git_ok(&r, &["push", "-q", "-u", "origin", "main"]);
    (server, r)
}

// ---------------------------------------------------------------------------

#[test]
fn history_shows_commits_and_their_details() {
    let s = scratch("history");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\n", "Start shop");
    commit(&r, "b.txt", "two\n", "Add cart page");
    let mut h = app(&s, &[&r], true);

    assert!(shows(&h, "Start shop") && shows(&h, "Add cart page"));
    click(&mut h, "Start shop");
    assert!(shows(&h, "✎ Edit message"));
    assert!(shows_containing(&h, "a.txt"));
}


#[test]
fn keyboard_search_sidebar_and_folding() {
    let s = scratch("nav");
    let r = new_repo(&s.root, "shop");
    for i in 1..=12 {
        commit(&r, &format!("f{i}.txt"), &format!("{i}\n"), &format!("Change number {i}"));
    }
    commit(&r, "two.txt", "x\n", "Edit two files");
    write(&r, "one.txt", "y\n");
    git_ok(&r, &["add", "-A"]);
    git_ok(&r, &["commit", "-q", "-m", "Edit another file"]);
    let mut h = app(&s, &[&r], true);

    // Clean working copy: the newest commit is selected and its files are shown.
    assert!(shows(&h, "Working directory is clean"));
    click(&mut h, "Edit two files");
    assert!(shows(&h, "Fold two.txt"));
    // Fold and unfold a file, then all of them.
    click(&mut h, "Fold two.txt");
    assert!(shows(&h, "Unfold two.txt"));
    click(&mut h, "Unfold two.txt");
    assert!(shows(&h, "Fold two.txt"));
    click(&mut h, "Collapse all");
    assert!(shows(&h, "Unfold two.txt"));
    click(&mut h, "Expand all");
    assert!(shows(&h, "Fold two.txt"));
    // Picking a file in the list scrolls to it (and unfolds it).
    click(&mut h, "Fold two.txt");
    click(&mut h, "two.txt");
    assert!(shows(&h, "Fold two.txt"));

    // Keys move through the history.
    key(&mut h, egui::Key::ArrowDown);
    key(&mut h, egui::Key::ArrowUp);
    key(&mut h, egui::Key::PageDown);
    key(&mut h, egui::Key::End);
    assert!(shows(&h, "Fold f1.txt"), "End selects the first commit ever");
    key(&mut h, egui::Key::PageUp);
    key(&mut h, egui::Key::Home);
    assert!(shows(&h, "Working directory is clean"));

    // Search narrows the list; Escape clears it.
    ctrl(&mut h, egui::Key::F);
    type_into(&mut h, "Search commits", "number 1");
    assert!(shows(&h, "Change number 1") && shows(&h, "Change number 12") && !shows(&h, "Change number 5"));
    key(&mut h, egui::Key::Escape);
    assert!(shows(&h, "Change number 5"));

    // The sidebar hides and shows with its button and with Ctrl+B.
    assert!(shows(&h, "main"));
    click(&mut h, "◧");
    assert!(!shows(&h, "Add remote…"));
    ctrl(&mut h, egui::Key::B);
    assert!(shows(&h, "Add remote…"));
    ctrl(&mut h, egui::Key::R);

    // A narrow window hides the extra columns but keeps working.
    h.set_size(egui::vec2(950.0, 700.0));
    settle(&mut h);
    assert!(shows(&h, "Change number 3"));
    h.set_size(egui::vec2(620.0, 600.0));
    settle(&mut h);
}

#[test]
fn edit_a_commit_message_from_details_and_menu() {
    let s = scratch("reword");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "1\n", "Frist commit");
    commit(&r, "b.txt", "2\n", "Second");
    let mut h = app(&s, &[&r], true);

    click(&mut h, "Frist commit");
    click(&mut h, "✎ Edit message");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "First commit");
    click(&mut h, "Save message");
    assert_eq!(subjects(&r, "main"), ["Second", "First commit"]);
    assert!(shows(&h, "Commit message updated"));
    assert!(shows(&h, "First commit"), "the edited commit is still shown");

    // From the right-click menu, on the latest commit; Cancel leaves it alone.
    right_click(&mut h, "Second");
    click(&mut h, "Edit commit message…");
    click(&mut h, "Cancel");
    right_click(&mut h, "Second");
    click(&mut h, "Edit commit message…");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "Second commit");
    ctrl(&mut h, egui::Key::Enter);
    assert_eq!(head_subject(&r), "Second commit");
}

/// Click the button named `button` that sits in the same row as `row`.
fn click_in_row(h: &mut Harness<'static, HeadwayApp>, row: &str, button: &str) {
    hover(h, row);
    let row_rect = h.get_all_by_label(row).next().unwrap().rect();
    h.get_all_by_label(button)
        .find(|b| row_rect.y_range().contains(b.rect().center().y))
        .unwrap_or_else(|| panic!("no “{button}” in the row “{row}”"))
        .click();
    settle(h);
}

/// Click the last control with this name (a confirmation's button sits on top of the page).
fn click_last(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label).last().unwrap_or_else(|| panic!("no “{label}” on screen")).click();
    settle(h);
}

fn staged_files(dir: &Path) -> Vec<String> {
    git_ok(dir, &["diff", "--cached", "--name-only"]).lines().map(String::from).collect()
}

#[test]
fn stage_unstage_discard_and_commit() {
    let s = scratch("stage");
    let r = new_repo(&s.root, "shop");
    let base: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    commit(&r, "a.txt", &base, "Start");
    commit(&r, "b.txt", "b\n", "Add b");
    // a.txt: two separate blocks of changes; b.txt changed; c.txt new.
    write(&r, "a.txt", &base.replace("line 2\n", "line 2\nNEW TOP\nNEW TOP 2\n").replace("line 28\n", "line 28\nNEW BOTTOM\n"));
    write(&r, "b.txt", "b changed\n");
    write(&r, "c.txt", "brand new\n");
    let mut h = app(&s, &[&r], true);
    assert!(shows(&h, "Uncommitted changes"));

    // A whole file from the list.
    click_in_row(&mut h, "b.txt", "Stage");
    assert_eq!(staged_files(&r), ["b.txt"]);
    click(&mut h, "Unstage all");
    assert!(staged_files(&r).is_empty());

    // Single lines: pick, extend with shift, clear with Escape, pick again, stage.
    click(&mut h, "line NEW TOP");
    h.get_all_by_label("line NEW TOP 2").next().unwrap().click_modifiers(egui::Modifiers::SHIFT);
    settle(&mut h);
    assert!(shows(&h, "2 lines picked"));
    key(&mut h, egui::Key::Escape);
    assert!(!shows(&h, "Stage lines"));
    click(&mut h, "line NEW TOP");
    h.get_all_by_label("line NEW TOP 2").next().unwrap().click_modifiers(egui::Modifiers::COMMAND);
    settle(&mut h);
    click(&mut h, "Clear");
    click(&mut h, "line NEW TOP");
    click(&mut h, "Stage lines");
    let cached = git_ok(&r, &["diff", "--cached"]);
    assert!(cached.contains("+NEW TOP\n") && !cached.contains("NEW TOP 2") && !cached.contains("NEW BOTTOM"));
    // …and take it back out: jump to the staged a.txt (the lower one in the list).
    click_last(&mut h, "a.txt");
    click_last(&mut h, "line NEW TOP");
    click(&mut h, "Unstage lines");
    assert!(staged_files(&r).is_empty());

    // Blocks (jump back to the top of the unstaged a.txt first).
    click(&mut h, "a.txt");
    click(&mut h, "Stage hunk");
    assert_eq!(staged_files(&r), ["a.txt"]);
    click(&mut h, "Unstage hunk");
    assert!(staged_files(&r).is_empty());
    // Discarding asks first; Cancel keeps the change.
    click(&mut h, "Discard hunk");
    click(&mut h, "Cancel");
    assert!(std::fs::read_to_string(r.join("a.txt")).unwrap().contains("NEW TOP"));
    click(&mut h, "Discard hunk");
    click_last(&mut h, "Discard");
    assert!(!std::fs::read_to_string(r.join("a.txt")).unwrap().contains("NEW TOP"));
    // Discard one line of what's left.
    click(&mut h, "line NEW BOTTOM");
    click(&mut h, "Discard lines");
    click_last(&mut h, "Discard");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), base);

    // A new file can be deleted from its name bar.
    write(&r, "junk.txt", "junk\n");
    ctrl(&mut h, egui::Key::R);
    let deletes = h.get_all_by_label("Delete").count();
    assert!(deletes >= 1);
    h.get_all_by_label("Delete").last().unwrap().click();
    settle(&mut h);
    click_last(&mut h, "Discard");
    assert!(!r.join("junk.txt").exists() || !r.join("c.txt").exists());

    // Stage everything and commit.
    click(&mut h, "Stage all");
    assert!(!staged_files(&r).is_empty());
    type_into(&mut h, "Describe your change", "Polish the shop");
    let commit_button = h.query_all_by_label_contains("Commit ").find(|n| format!("{n:?}").contains("to main")).map(|_| ());
    assert!(commit_button.is_some());
    h.get_all_by_label_contains("to main").next().unwrap().click();
    settle(&mut h);
    assert_eq!(head_subject(&r), "Polish the shop");
    assert!(shows(&h, "Working directory is clean"));

    // Amend: the box fills with the last message; change it and amend.
    click(&mut h, "Working directory is clean");
    click(&mut h, "Amend last commit");
    click_text_containing(&mut h, "Polish the shop");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "Polish the shop, amended");
    click(&mut h, "Amend commit");
    assert_eq!(head_subject(&r), "Polish the shop, amended");
    assert_eq!(subjects(&r, "main").len(), 3, "amended, not added");

    // Nothing staged: "Stage all & commit" does both. Ctrl+Enter commits from the box.
    write(&r, "b.txt", "b again\n");
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Uncommitted changes");
    type_into(&mut h, "Describe your change", "Change b again");
    click(&mut h, "Stage all & commit");
    assert_eq!(head_subject(&r), "Change b again");
    write(&r, "b.txt", "b once more\n");
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Uncommitted changes");
    double_click(&mut h, "b.txt");
    assert_eq!(staged_files(&r), ["b.txt"], "double-click stages");
    type_into(&mut h, "Describe your change", "Once more");
    ctrl(&mut h, egui::Key::Enter);
    assert_eq!(head_subject(&r), "Once more");
}

fn branches(dir: &Path) -> Vec<String> {
    git_ok(dir, &["branch", "--format=%(refname:short)"]).lines().map(String::from).collect()
}

#[test]
fn branches_tags_stashes_and_the_commit_menu() {
    let s = scratch("branches");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "1\n", "Start shop");
    git_ok(&r, &["branch", "topic"]);
    commit(&r, "b.txt", "2\n", "Add cart page");
    git_ok(&r, &["checkout", "-q", "topic"]);
    commit(&r, "t.txt", "t\n", "Topic work");
    git_ok(&r, &["checkout", "-q", "main"]);
    git_ok(&r, &["tag", "v1"]);
    let mut h = app(&s, &[&r], true);

    // Click selects the branch's commit; double-click switches to it.
    click(&mut h, "topic");
    assert!(shows(&h, "Fold t.txt"));
    double_click(&mut h, "topic");
    assert_eq!(branch(&r), "topic");
    // Switch back with the branch picker.
    click(&mut h, "Current branch: topic");
    click(&mut h, "main"); // the list opens above the sidebar's own "main"
    assert_eq!(branch(&r), "main");

    // New branch from the toolbar.
    click(&mut h, "⑂  Branch");
    type_text(&mut h, "feature/login");
    click(&mut h, "Create branch");
    assert_eq!(branch(&r), "feature/login");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);

    // Right-click a branch: merge, rebase, new branch, copy, delete.
    right_click(&mut h, "topic");
    click(&mut h, "Merge topic into main");
    click_last(&mut h, "Merge");
    assert!(subjects(&r, "main").iter().any(|s| s.starts_with("Merge branch 'topic'")));
    right_click(&mut h, "topic");
    click(&mut h, "Copy name");
    right_click(&mut h, "topic");
    click(&mut h, "New branch from here…");
    click(&mut h, "Switch to the new branch");
    type_text(&mut h, "from-topic");
    click(&mut h, "Create branch");
    assert!(branches(&r).contains(&"from-topic".to_string()));
    assert_eq!(branch(&r), "main", "the box was unticked, so we stayed");
    right_click(&mut h, "from-topic");
    click(&mut h, "Delete…");
    click_last(&mut h, "Delete");
    assert!(!branches(&r).contains(&"from-topic".to_string()));
    // Rebase main onto a branch with a new commit.
    git_ok(&r, &["branch", "base2", "main"]);
    git_ok(&r, &["checkout", "-q", "base2"]);
    commit(&r, "z.txt", "z\n", "Base work");
    git_ok(&r, &["checkout", "-q", "main"]);
    commit(&r, "m.txt", "m\n", "Main work");
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, "base2");
    click(&mut h, "Rebase main onto base2");
    click_last(&mut h, "Rebase");
    assert_eq!(subjects(&r, "main")[..2], ["Main work", "Base work"]);
    // Force delete an unmerged branch.
    git_ok(&r, &["checkout", "-q", "-b", "unmerged"]);
    commit(&r, "u.txt", "u\n", "Unmerged");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, "unmerged");
    click(&mut h, "Delete…");
    click_last(&mut h, "Delete");
    assert!(branches(&r).contains(&"unmerged".to_string()), "not merged: plain delete refuses");
    assert!(shows_containing(&h, "failed"));
    right_click(&mut h, "unmerged");
    click(&mut h, "Force delete…");
    click_last(&mut h, "Force delete");
    assert!(!branches(&r).contains(&"unmerged".to_string()));

    // Hide a branch and its own commits, then bring it back.
    click(&mut h, "Hide topic");
    assert!(shows(&h, "Show 1 hidden branch"));
    click(&mut h, "Show topic");
    click(&mut h, "Hide topic");
    click(&mut h, "Show 1 hidden branch");
    assert!(shows(&h, "Hide topic"));

    // Tags: open the section, then use the menu.
    click(&mut h, "TAGS   1");
    click(&mut h, "v1");
    right_click(&mut h, "v1");
    click(&mut h, "Copy name");
    right_click(&mut h, "v1");
    click(&mut h, "Check out this tag");
    click_last(&mut h, "Check out");
    assert_eq!(branch(&r), "", "detached at the tag");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, "v1");
    click(&mut h, "Delete…");
    click_last(&mut h, "Delete");
    assert!(git_ok(&r, &["tag"]).trim().is_empty());

    // Stash and bring back, from the toolbar and the list.
    write(&r, "a.txt", "dirty\n");
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "⤓  Stash");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "1\n");
    click(&mut h, "⤒  Pop");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "dirty\n");
    click(&mut h, "⤓  Stash");
    let stash = git_ok(&r, &["stash", "list", "--format=%gs"]).trim().to_string();
    right_click(&mut h, &stash);
    click(&mut h, "Apply (keep stash)");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "dirty\n");
    git_ok(&r, &["checkout", "--", "a.txt"]);
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, &stash);
    click(&mut h, "Pop (apply and remove)");
    git_ok(&r, &["stash", "push", "-q"]);
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, &stash);
    click(&mut h, "Drop…");
    click_last(&mut h, "Drop");
    assert!(git_ok(&r, &["stash", "list"]).trim().is_empty());

    // The commit menu.
    right_click_row(&mut h, "Add cart page");
    click(&mut h, "New tag here…");
    type_text(&mut h, "v2");
    click(&mut h, "Create tag");
    assert_eq!(git_ok(&r, &["tag"]).trim(), "v2");
    right_click_row(&mut h, "Add cart page");
    click(&mut h, "New branch here…");
    type_text(&mut h, "from-commit");
    click(&mut h, "Create branch");
    assert_eq!(branch(&r), "from-commit");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);
    for item in ["Copy commit id", "Copy message"] {
        right_click_row(&mut h, "Add cart page");
        click(&mut h, item);
    }
    right_click_row(&mut h, "Topic work");
    click(&mut h, "Copy this commit onto main (cherry-pick)");
    click(&mut h, "Cancel");
    right_click_row(&mut h, "Base work");
    click(&mut h, "Undo this commit with a new commit (revert)");
    click_last(&mut h, "Revert");
    assert_eq!(head_subject(&r), "Revert \"Base work\"");
    for (item, confirm) in [("Keep all changes, staged", "Reset"), ("Keep all changes, unstaged", "Reset"), ("Discard everything after it", "Reset")] {
        right_click_row(&mut h, "Start shop");
        hover(&mut h, "Move main to this commit");
        click(&mut h, item);
        click_last(&mut h, confirm);
        assert_eq!(head_subject(&r), "Start shop", "{item}");
        git_ok(&r, &["reset", "-q", "--hard", "ORIG_HEAD"]);
        ctrl(&mut h, egui::Key::R);
    }
    right_click_row(&mut h, "Start shop");
    click(&mut h, "Check out this commit…");
    click_last(&mut h, "Check out");
    assert_eq!(head_subject(&r), "Start shop");
    assert_eq!(branch(&r), "");
}

#[test]
fn remotes_fetch_pull_and_push() {
    let s = scratch("remotes");
    let (server, r) = with_server(&s);
    // Someone else pushes a commit and a branch.
    let other = s.root.join("other");
    git_ok(&s.root, &["clone", "-q", server.to_str().unwrap(), other.to_str().unwrap()]);
    git_ok(&other, &["config", "user.name", "Sam"]);
    git_ok(&other, &["config", "user.email", "sam@example.com"]);
    commit(&other, "c.txt", "c\n", "Their change");
    git_ok(&other, &["push", "-q", "origin", "main"]);
    git_ok(&other, &["push", "-q", "origin", "main:server-only"]);
    let mut h = app(&s, &[&r], true);

    click(&mut h, "⇅  Fetch");
    assert!(shows_containing(&h, "Pull  1"), "one commit to pull");
    h.get_all_by_label_contains("Pull").next().unwrap().click();
    settle(&mut h);
    assert_eq!(head_subject(&r), "Their change");

    // Push a new commit; publish a new branch.
    commit(&r, "d.txt", "d\n", "My change");
    ctrl(&mut h, egui::Key::R);
    h.get_all_by_label_contains("Push").next().unwrap().click();
    settle(&mut h);
    assert_eq!(git_ok(&server, &["log", "-1", "--format=%s", "main"]).trim(), "My change");
    git_ok(&r, &["checkout", "-q", "-b", "fresh"]);
    ctrl(&mut h, egui::Key::R);
    h.get_all_by_label_contains("Push").next().unwrap().click();
    settle(&mut h);
    assert!(git_ok(&server, &["branch"]).contains("fresh"));
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);

    // Remote branches: check one out, merge one, branch from one.
    click(&mut h, "server-only");
    right_click(&mut h, "server-only");
    click(&mut h, "Merge origin/server-only into main");
    right_click(&mut h, "server-only");
    click(&mut h, "New branch from here…");
    type_text(&mut h, "from-remote");
    click(&mut h, "Create branch");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);
    double_click(&mut h, "server-only");
    assert_eq!(branch(&r), "server-only");
    git_ok(&r, &["checkout", "-q", "main"]);
    ctrl(&mut h, egui::Key::R);
    right_click(&mut h, "fresh");
    click(&mut h, "Push");

    // The remote's own menu, and adding/removing remotes.
    right_click(&mut h, "☁  origin");
    click(&mut h, "Fetch origin");
    right_click(&mut h, "☁  origin");
    click(&mut h, "Copy address");
    click(&mut h, "Add remote…");
    type_into(&mut h, "upstream", "fork");
    type_into(&mut h, "https://example.com/team/project.git", server.to_str().unwrap());
    click_last(&mut h, "Add remote"); // the button, not the dialog's title
    assert_eq!(git::remotes(&r).unwrap(), ["fork", "origin"]);
    right_click(&mut h, "☁  fork");
    click(&mut h, "Remove fork…");
    click_last(&mut h, "Remove");
    assert_eq!(git::remotes(&r).unwrap(), ["origin"]);
}

#[test]
fn overview_workspaces_tabs_and_dialogs() {
    let s = scratch("overview");
    let (_server, a) = with_server(&s);
    let b = new_repo(&s.root, "notes");
    commit(&b, "n.txt", "n\n", "First note");
    write(&b, "n.txt", "changed\n");
    git_ok(&a, &["worktree", "add", "-q", "-b", "feature/x", s.root.join("shop-feature-x").to_str().unwrap()]);
    let c = new_repo(&s.root, "extra");
    commit(&c, "e.txt", "e\n", "Extra start");
    let mut h = app(&s, &[&a, &b], false);

    // The overview: cards, a worktree box, the header buttons.
    assert!(shows(&h, "Card shop") && shows(&h, "Card notes") && shows(&h, "Worktree feature/x"));
    click(&mut h, "⇅  Fetch all");
    click(&mut h, "⬇  Pull all");
    click(&mut h, "⟳  Refresh");
    let fetches = h.get_all_by_label("Fetch").count();
    assert!(fetches >= 1);
    h.get_all_by_label("Fetch").find(|n| !n.accesskit_node().is_disabled()).unwrap().click();
    settle(&mut h);
    h.get_all_by_label("Pull").find(|n| !n.accesskit_node().is_disabled()).unwrap().click();
    settle(&mut h);

    // Add a repository by typing its folder.
    click(&mut h, "+  Add");
    click(&mut h, "Type a folder path…");
    type_text(&mut h, c.to_str().unwrap());
    click_last(&mut h, "Add");
    assert!(shows(&h, "Close extra"), "a single repository opens straight away");
    click(&mut h, "⌂  Overview");
    assert!(shows(&h, "Card extra"));
    // A plain folder offers to become a repository.
    let plain = s.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    click(&mut h, "+  Add");
    click(&mut h, "Type a folder path…");
    type_text(&mut h, plain.to_str().unwrap());
    click_last(&mut h, "Add");
    click(&mut h, "Create repository");
    assert!(plain.join(".git").exists());
    // Clone from a "web address" (a local path works the same).
    click(&mut h, "+  Add");
    click(&mut h, "Clone from a web address…");
    type_into(&mut h, "https://example.com/team/project.git", s.root.join("server.git").to_str().unwrap());
    // The destination box starts with the home folder.
    click_text_containing(&mut h, &dirs::home_dir().unwrap().display().to_string());
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, s.root.join("clones").to_str().unwrap());
    std::fs::create_dir_all(s.root.join("clones")).unwrap();
    settle(&mut h);
    click_last(&mut h, "Clone");
    assert!(s.root.join("clones/server/.git").exists());
    assert!(shows(&h, "Close server"), "the clone opens straight away");
    click(&mut h, "⌂  Overview");

    // A card's menu, and removing a repository from the workspace.
    let menus = h.get_all_by_label("⋯").count();
    assert!(menus >= 3);
    h.get_all_by_label("⋯").next().unwrap().click();
    settle(&mut h);
    click(&mut h, "Copy folder path");
    h.get_all_by_label("⋯").nth(1).unwrap().click();
    settle(&mut h);
    click(&mut h, "Remove from workspace");
    assert!(!shows(&h, "Card notes"));

    // Opening: a card, a worktree box, and tabs.
    click(&mut h, "Worktree feature/x");
    assert!(shows(&h, "shop-feature-x"));
    click(&mut h, "⌂  Overview");
    // The worktree box has its own "Open"; the card's is the last one in it.
    let card = h.get_by_label("Card shop").rect();
    h.get_all_by_label("Open").filter(|n| card.contains(n.rect().center())).last().unwrap().click();
    settle(&mut h);
    assert!(shows(&h, "Close shop"));
    click(&mut h, "Close shop-feature-x");
    click(&mut h, "Close shop");
    assert!(!shows(&h, "Close shop"));
    click(&mut h, "⌂  Overview");
    double_click(&mut h, "Card extra");
    assert!(shows(&h, "Close extra"));
    click(&mut h, "⌂  Overview");
    // Clicking a project in the sidebar's workspace list opens it too.
    click(&mut h, "extra");
    click(&mut h, "shop");
    assert!(shows(&h, "Close shop"));
    click(&mut h, "⌂  Overview");

    // Workspaces: new, rename, switch, delete.
    click(&mut h, "▦  Office  ⏷");
    click(&mut h, "New workspace…");
    type_text(&mut h, "Client");
    click(&mut h, "Save");
    assert!(shows(&h, "This workspace is empty"));
    click(&mut h, "▦  Client  ⏷");
    click(&mut h, "Rename this workspace…");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "Client work");
    key(&mut h, egui::Key::Enter);
    assert!(shows(&h, "▦  Client work  ⏷"));
    click(&mut h, "▦  Client work  ⏷");
    h.get_all_by_label_contains("Office").find(|n| n.accesskit_node().label().is_some_and(|l| l.contains("repos"))).unwrap().click();
    settle(&mut h);
    assert!(shows(&h, "Card shop"));
    click(&mut h, "▦  Office  ⏷");
    click(&mut h, "Delete this workspace…");
    click(&mut h, "Cancel");
    click(&mut h, "▦  Office  ⏷");
    click(&mut h, "Delete this workspace…");
    click(&mut h, "Delete workspace");
    assert!(shows(&h, "▦  Client work  ⏷"));
    // Escape closes a dialog.
    click(&mut h, "▦  Client work  ⏷");
    click(&mut h, "New workspace…");
    key(&mut h, egui::Key::Escape);
    assert!(!shows(&h, "Save"));
    // The settings were saved to the test's own file, not the real one.
    let saved = std::fs::read_to_string(s.root.join("settings.json")).unwrap();
    assert!(saved.contains("Client work") && !saved.contains("\"Office\""));
}

#[test]
fn worktrees_from_the_sidebar() {
    let s = scratch("worktrees");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "1\n", "Start shop");
    git_ok(&r, &["branch", "spare"]);
    let mut h = app(&s, &[&r], true);

    // New worktree with a new branch.
    click(&mut h, "WORKTREES   1");
    click(&mut h, "New worktree…");
    type_into(&mut h, "feature/my-change", "feature/cart");
    click_last(&mut h, "Create worktree");
    let wt = s.root.join("shop-feature-cart");
    assert!(wt.join("a.txt").exists());
    assert!(shows(&h, "Close shop-feature-cart"), "opened in a tab");
    click(&mut h, "Close shop-feature-cart");
    click(&mut h, "shop");

    // A branch checked out elsewhere opens its worktree instead of switching.
    double_click(&mut h, "feature/cart");
    assert!(shows(&h, "Close shop-feature-cart"));
    click(&mut h, "Close shop-feature-cart");
    click(&mut h, "shop");
    right_click(&mut h, "feature/cart");
    click(&mut h, "Open its worktree");
    click(&mut h, "Close shop-feature-cart");
    click(&mut h, "shop");

    // New worktree for an existing branch, from the branch's menu.
    right_click(&mut h, "spare");
    click(&mut h, "New worktree for spare…");
    click_last(&mut h, "Create worktree");
    assert!(s.root.join("shop-spare").exists());
    click(&mut h, "Close shop-spare");
    click(&mut h, "shop");

    // The worktree list: open, copy, remove; clean up a vanished one.
    click(&mut h, "spare");
    // The worktree row comes after the branch row with the same name.
    h.get_all_by_label("feature/cart").last().unwrap().click_secondary();
    settle(&mut h);
    click(&mut h, "Copy folder path");
    let rows: Vec<String> = h.get_all_by_role(egui::Role::Button).filter_map(|n| n.accesskit_node().label().map(|l| l.to_string())).collect();
    assert!(rows.contains(&"feature/cart".to_string()));
    write(&wt, "dirty.txt", "x\n");
    h.get_all_by_label("feature/cart").last().unwrap().click_secondary();
    settle(&mut h);
    click(&mut h, "Remove…");
    click_last(&mut h, "Remove");
    assert!(wt.exists(), "a worktree with changes isn't removed without force");
    h.get_all_by_label("feature/cart").last().unwrap().click_secondary();
    settle(&mut h);
    click(&mut h, "Force remove…");
    click_last(&mut h, "Remove");
    assert!(!wt.exists());
    std::fs::remove_dir_all(s.root.join("shop-spare")).unwrap();
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Clean up missing worktrees");
    assert_eq!(git::worktrees(&r).unwrap().len(), 1);

    // From a commit's menu.
    right_click_row(&mut h, "Start shop");
    click(&mut h, "New worktree here…");
    type_into(&mut h, "feature/my-change", "from-commit");
    click_last(&mut h, "Create worktree");
    assert!(s.root.join("shop-from-commit").exists());
}

#[test]
fn submodules_from_the_sidebar() {
    // SAFETY: local paths are blocked for submodules by default; allow them in tests.
    unsafe { std::env::set_var("GIT_CONFIG_PARAMETERS", "'protocol.file.allow=always'") };
    let s = scratch("submodules");
    let lib = new_repo(&s.root, "ui-kit");
    commit(&lib, "kit.txt", "v1\n", "Kit v1");
    let r = new_repo(&s.root, "app");
    commit(&r, "a.txt", "1\n", "Start app");
    let mut h = app(&s, &[&r], true);

    click(&mut h, "SUBMODULES");
    click(&mut h, "Add submodule…");
    type_into(&mut h, "shared-lib.git", lib.to_str().unwrap());
    click_last(&mut h, "Add submodule");
    assert!(r.join("ui-kit/kit.txt").exists());
    git_ok(&r, &["commit", "-q", "-m", "Add ui-kit"]);
    ctrl(&mut h, egui::Key::R);

    // Open it; copy things; update to its latest commit; reset back.
    click(&mut h, "ui-kit");
    assert!(shows(&h, "Close ui-kit"));
    click(&mut h, "Close ui-kit");
    click(&mut h, "app");
    for item in ["Copy folder path", "Copy address"] {
        right_click(&mut h, "ui-kit");
        click(&mut h, item);
    }
    commit(&lib, "kit.txt", "v2\n", "Kit v2");
    right_click(&mut h, "ui-kit");
    click(&mut h, "Update to its latest commit");
    assert_eq!(head_subject(&r.join("ui-kit")), "Kit v2");
    // The changes view explains it and offers Open/Reset on its name bar.
    click(&mut h, "Uncommitted changes");
    assert!(shows(&h, "Open submodule") && shows(&h, "Reset"));
    click(&mut h, "Reset");
    click_last(&mut h, "Discard");
    assert_eq!(head_subject(&r.join("ui-kit")), "Kit v1");
    right_click(&mut h, "ui-kit");
    click(&mut h, "Update to its latest commit");
    right_click(&mut h, "ui-kit");
    click(&mut h, "Reset to recorded commit");
    assert_eq!(head_subject(&r.join("ui-kit")), "Kit v1");
    // Not set up yet → "Update all submodules" sets it up.
    git_ok(&r, &["submodule", "deinit", "-q", "-f", "ui-kit"]);
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Update all submodules");
    assert!(r.join("ui-kit/kit.txt").exists());
    // Remove it.
    right_click(&mut h, "ui-kit");
    click(&mut h, "Remove submodule…");
    click_last(&mut h, "Remove");
    assert!(!r.join("ui-kit").exists());
    // The overview shows submodule counts.
    git_ok(&r, &["commit", "-q", "-m", "Remove ui-kit"]);
    git_ok(&r, &["submodule", "-q", "add", lib.to_str().unwrap(), "libs/kit"]);
    git_ok(&r, &["commit", "-q", "-m", "Add kit again"]);
    git_ok(&r, &["submodule", "deinit", "-q", "-f", "libs/kit"]);
    click(&mut h, "⌂  Overview");
    click(&mut h, "⟳  Refresh");
    assert!(shows_containing(&h, "submodules not set up"));
}

#[test]
fn missing_folders_empty_workspaces_and_long_histories() {
    let s = scratch("edges");
    let r = new_repo(&s.root, "big");
    // 3,001 commits, quickly.
    let mut stream = String::new();
    for i in 1..=3001 {
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter A <a@x> {} +0000\ndata <<EOM\nCommit {i}\nEOM\n{}M 644 inline f.txt\ndata <<EOM\n{i}\nEOM\n\n",
            1_700_000_000 + i,
            if i > 1 { "" } else { "" }
        ));
    }
    git::run_stdin(&r, &["fast-import", "--quiet"], &stream).unwrap();
    git_ok(&r, &["checkout", "-q", "-f", "main"]);
    let gone = s.root.join("gone");
    let mut h = app(&s, &[&r, &gone], true);

    key(&mut h, egui::Key::End);
    assert!(shows(&h, "Load more history"));
    click(&mut h, "Load more history");
    assert!(!shows(&h, "Load more history"));
    // Searching by tag/author/id works on the long list too.
    type_into(&mut h, "Search commits", "Commit 3000");
    assert!(shows(&h, "Commit 3000"));

    // A missing folder shows an error with "Try again"; the overview flags it.
    click(&mut h, "gone");
    assert!(shows(&h, "Try again"));
    click(&mut h, "Try again");
    click(&mut h, "⌂  Overview");
    assert!(shows_containing(&h, "Not a git repository"));

    // Coming back to the window refreshes things.
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().focused = Some(false);
    settle(&mut h);
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().focused = Some(true);
    settle(&mut h);

    // An empty workspace explains itself.
    let mut empty = app(&s, &[], false);
    assert!(shows(&empty, "This workspace is empty"));
    click(&mut empty, "Clone from a web address…");
    click(&mut empty, "Cancel");
}

/// Press on `from`, move to `to` over a few frames, release there.
fn drag(h: &mut Harness<'static, HeadwayApp>, from: egui::Pos2, to: egui::Pos2) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for i in 1..=4 {
        h.hover_at(from + (to - from) * (i as f32 / 4.0));
        h.step();
    }
    h.drop_at(to);
    settle(h);
}

#[test]
fn conflicts_renames_binary_files_and_finer_controls() {
    let s = scratch("finer");
    let r = new_repo(&s.root, "shop");
    commit(&r, "c.txt", "base\n", "Start");
    commit(&r, "old.txt", "rename me\n", "Add old");
    commit(&r, "nonl.txt", "a\nb", "No final newline");
    let lines: String = (1..=10).map(|i| format!("row {i}\n")).collect();
    commit(&r, "rows.txt", &lines, "Rows");
    git_ok(&r, &["checkout", "-q", "-b", "other"]);
    commit(&r, "c.txt", "theirs\n", "Theirs");
    git_ok(&r, &["checkout", "-q", "main"]);
    commit(&r, "c.txt", "ours\n", "Ours");
    assert!(git::run(&r, &["merge", "other"]).is_err(), "conflict");
    git_ok(&r, &["mv", "old.txt", "new.txt"]);
    std::fs::write(r.join("pic.bin"), [0u8, 1, 2, 0, 255, 0, 7]).unwrap();
    git_ok(&r, &["add", "pic.bin"]);
    write(&r, "nonl.txt", "a\nB");
    write(&r, "rows.txt", &lines.replace("row 3\n", "ROW 3\n").replace("row 4\n", "ROW 4\n").replace("row 5\n", "ROW 5\n"));
    let mut h = app(&s, &[&r], true);

    // A conflict is listed first; marking it resolved stages it.
    assert!(shows(&h, "c.txt"));
    click_in_row(&mut h, "c.txt", "Mark resolved");
    assert!(staged_files(&r).contains(&"c.txt".to_string()));
    // Renamed and binary files show up staged; unstage one from the list and one from its name bar.
    assert!(shows(&h, "new.txt") && shows(&h, "pic.bin"));
    click_in_row(&mut h, "pic.bin", "Unstage");
    assert!(!staged_files(&r).contains(&"pic.bin".to_string()));
    click(&mut h, "pic.bin");
    double_click(&mut h, "new.txt");
    assert!(!staged_files(&r).contains(&"new.txt".to_string()), "double-click unstages");
    git_ok(&r, &["add", "-A", "new.txt", "old.txt"]);
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "c.txt");
    let unstage = h.get_all_by_label("Unstage").last().unwrap();
    unstage.click();
    settle(&mut h);
    // Fold everything in the changes view, then unfold.
    click(&mut h, "Collapse all");
    click(&mut h, "Expand all");
    // A file's name bar can discard it.
    click(&mut h, "nonl.txt");
    click_in_row(&mut h, "nonl.txt", "Discard");
    click(&mut h, "Cancel");

    // Drag across lines to pick several.
    click(&mut h, "rows.txt");
    let a = h.get_by_label("line ROW 3").rect().center();
    let b = h.get_by_label("line ROW 5").rect().center();
    drag(&mut h, a, b);
    assert!(shows(&h, "3 lines picked"));
    key(&mut h, egui::Key::Escape);

    // The divider between history and details: drag it, double-click to halve.
    let d = h.get_by_label("Resize history and details").rect().center();
    drag(&mut h, d, d - egui::vec2(200.0, 0.0));
    double_click(&mut h, "Resize history and details");

    // Notifications can be clicked away.
    click(&mut h, "⇅  Fetch");
    let toast = h.query_all_by_label_contains("done").next().is_some();
    if toast {
        h.get_all_by_label_contains("done").next().unwrap().click();
        settle(&mut h);
    }
    // Middle-click closes a tab.
    h.get_all_by_label("shop").find(|n| n.accesskit_node().role() == egui::Role::Tab).unwrap().click_button(egui::PointerButton::Middle);
    settle(&mut h);
    assert!(shows(&h, "Card shop"));
}

#[test]
fn details_overview_badges_and_sidebar_extras() {
    let s = scratch("extras");
    let (server, r) = with_server(&s);
    // Ahead and behind at once.
    let other = s.root.join("other");
    git_ok(&s.root, &["clone", "-q", server.to_str().unwrap(), other.to_str().unwrap()]);
    git_ok(&other, &["config", "user.name", "Sam"]);
    git_ok(&other, &["config", "user.email", "sam@example.com"]);
    commit(&other, "x.txt", "x\n", "Theirs");
    git_ok(&other, &["push", "-q", "origin", "main"]);
    git_ok(&r, &["fetch", "-q"]);
    write(&r, "y.txt", "y\n");
    git_ok(&r, &["add", "y.txt"]);
    git_ok(&r, &["-c", "user.name=Committer Person", "commit", "-q", "--author=Ana Lima <ana@example.com>", "-m", "Mine\n\nWith a longer body explaining it."]);
    git_ok(&r, &["tag", "v1"]);
    // A locked worktree, a detached one, and one whose folder is gone.
    git_ok(&r, &["worktree", "add", "-q", "-b", "locked-branch", s.root.join("wt-locked").to_str().unwrap()]);
    git_ok(&r, &["worktree", "lock", s.root.join("wt-locked").to_str().unwrap()]);
    git_ok(&r, &["worktree", "add", "-q", "--detach", s.root.join("wt-detached").to_str().unwrap()]);
    git_ok(&r, &["worktree", "add", "-q", "-b", "gone-branch", s.root.join("wt-gone").to_str().unwrap()]);
    std::fs::remove_dir_all(s.root.join("wt-gone")).unwrap();
    // A standalone worktree of a repository that isn't in the workspace.
    let lone_main = new_repo(&s.root, "lonely");
    commit(&lone_main, "l.txt", "l\n", "Lonely");
    let lone_wt = s.root.join("lonely-wt");
    git_ok(&lone_main, &["worktree", "add", "-q", "-b", "a-very-long-branch-name-that-needs-cutting-short-in-a-tag", lone_wt.to_str().unwrap()]);
    let mut h = app(&s, &[&r, &lone_wt], false);

    // Overview badges.
    assert!(shows_containing(&h, "1 to push") && shows_containing(&h, "1 to pull"));
    assert!(shows(&h, "Worktree locked-branch"));
    h.get_all_by_label("⋯").next().unwrap().click();
    settle(&mut h);
    click(&mut h, "Open");
    click(&mut h, "⌂  Overview");
    click(&mut h, "Worktree locked-branch");
    click(&mut h, "⌂  Overview");
    hover(&mut h, "Worktree gone-branch");
    let card = h.get_by_label("Card shop").rect();
    h.get_all_by_label("Open").filter(|n| card.contains(n.rect().center())).last().unwrap().click();
    settle(&mut h);

    // Commit details: body, committer, parent link, copy.
    click(&mut h, "Mine");
    assert!(shows_containing(&h, "committed by Committer Person"));
    click(&mut h, "Copy");
    let parent = git_ok(&r, &["rev-parse", "--short=8", "HEAD~1"]).trim().to_string();
    click(&mut h, &parent);

    // Toolbar branch picker extras.
    click(&mut h, "Current branch: main");
    click(&mut h, "New branch…");
    click(&mut h, "Cancel");
    click(&mut h, "Current branch: main");
    click(&mut h, "New worktree…");
    click(&mut h, "Cancel");
    click(&mut h, "Current branch: main");
    hover(&mut h, "locked-branch");
    click_last(&mut h, "locked-branch");
    // Sidebar worktree rows: locked, detached, gone; open one from its menu.
    click(&mut h, "shop");
    hover(&mut h, "wt-detached");
    // Worktree rows are named after their folder when it isn't "<project>-<branch>";
    // the workspace list above may show the same name, so take the last one.
    let wt_row = |h: &Harness<'static, HeadwayApp>| h.get_all_by_label("wt-locked").filter(|n| n.accesskit_node().role() == egui::Role::Button).last().unwrap().rect().center();
    h.hover_at(wt_row(&h));
    settle(&mut h);
    h.get_all_by_label("wt-locked").filter(|n| n.accesskit_node().role() == egui::Role::Button).last().unwrap().click_secondary();
    settle(&mut h);
    click(&mut h, "Open");
    click(&mut h, "shop");
    h.get_all_by_label("wt-locked").filter(|n| n.accesskit_node().role() == egui::Role::Button).last().unwrap().click();
    settle(&mut h);
    click(&mut h, "shop");
    // Remote branch: hide it; check out "main" when a local main exists.
    // The current branch's eye is greyed out; the remote "main" can be hidden.
    assert!(h.get_all_by_label("Hide main").next().unwrap().accesskit_node().is_disabled());
    h.get_all_by_label("Hide main").find(|n| !n.accesskit_node().is_disabled()).unwrap().click();
    settle(&mut h);
    click(&mut h, "Show 1 hidden branch");
    let remote_main = h.get_all_by_label("main").last().unwrap();
    remote_main.click_secondary();
    settle(&mut h);
    click(&mut h, "Check out as “main”");
    // Tags: push one.
    click(&mut h, "TAGS   1");
    right_click(&mut h, "v1");
    click(&mut h, "Push to origin");
    assert!(git_ok(&server, &["tag"]).contains("v1"));
    // Selecting a hidden branch explains where it went.
    click(&mut h, "Hide locked-branch");
    click(&mut h, "locked-branch");
    assert!(shows_containing(&h, "is hidden"));
}

#[test]
fn dialog_edges_errors_and_bulk_adding() {
    let s = scratch("dialogs");
    let (_server, r) = with_server(&s);
    git_ok(&r, &["branch", "spare"]);
    let nest = s.root.join("nest");
    for n in ["one", "two"] {
        let d = new_repo(&nest, n);
        commit(&d, "f.txt", "f\n", n);
    }
    let mut h = app(&s, &[&r], true);

    // Every dialog's Cancel and its warnings.
    click(&mut h, "Add remote…");
    type_into(&mut h, "upstream", "origin");
    assert!(shows(&h, "There's already a remote with that name."));
    click(&mut h, "Cancel");
    click(&mut h, "WORKTREES   1");
    click(&mut h, "New worktree…");
    click(&mut h, "Existing branch");
    click(&mut h, "Branch for the worktree: spare");
    click_last(&mut h, "spare");
    click_text_containing(&mut h, "shop-spare");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, s.root.to_str().unwrap());
    assert!(shows(&h, "That folder already exists and isn't empty."));
    click(&mut h, "Cancel");
    click(&mut h, "SUBMODULES");
    click(&mut h, "Add submodule…");
    type_into(&mut h, "shared-lib.git", "https://example.com/x/lib.git");
    click_text_containing(&mut h, "lib");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "a.txt");
    assert!(shows(&h, "Something already exists at that folder."));
    click(&mut h, "Cancel");
    right_click_row(&mut h, "Start shop");
    click(&mut h, "Edit commit message…");
    assert!(shows_containing(&h, "already on a remote"));
    click(&mut h, "Cancel");
    right_click_row(&mut h, "Start shop");
    click(&mut h, "New tag here…");
    click(&mut h, "Cancel");
    click(&mut h, "⑂  Branch");
    click(&mut h, "Cancel");

    // A folder holding several repositories adds them all.
    click(&mut h, "⌂  Overview");
    click(&mut h, "+  Add");
    click(&mut h, "Type a folder path…");
    type_text(&mut h, nest.to_str().unwrap());
    click_last(&mut h, "Add");
    assert!(shows(&h, "Card one") && shows(&h, "Card two"));
    click(&mut h, "+  Add");
    click(&mut h, "Type a folder path…");
    click(&mut h, "Cancel");
    // A clone that can't work explains why.
    click(&mut h, "+  Add");
    click(&mut h, "Clone from a web address…");
    type_into(&mut h, "https://example.com/team/project.git", s.root.join("no-such-repo").to_str().unwrap());
    click_last(&mut h, "Clone");
    assert!(shows_containing(&h, "Clone failed"));
    click(&mut h, "+  Add");
    click(&mut h, "Clone from a web address…");
    click(&mut h, "Cancel");
    click(&mut h, "▦  Office  ⏷");
    click(&mut h, "Rename this workspace…");
    click(&mut h, "Cancel");
    // Removing a repository whose tab is open closes the tab too.
    let card = h.get_by_label("Card shop").rect();
    h.get_all_by_label("⋯").find(|n| card.contains(n.rect().center())).unwrap().click();
    settle(&mut h);
    click(&mut h, "Remove from workspace");
    assert!(!shows(&h, "Card shop"));

    // Plain-language errors.
    for (raw, hint) in [
        ("fatal: Not possible to fast-forward, aborting.", "both changed"),
        ("CONFLICT (content): Merge conflict in a.txt", "There are conflicts"),
        ("fatal: The current branch x has no upstream branch.", "Push it first"),
    ] {
        assert!(super::friendly_error(raw).contains(hint), "{raw}");
    }
}

/// Hand the app a message as if a background job (e.g. the system folder picker) sent it.
fn inject(h: &mut Harness<'static, HeadwayApp>, msg: super::Msg) {
    h.state().pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    h.state().tx.send(msg).unwrap();
    settle(h);
}

#[test]
fn folder_picker_answers_odd_requests_and_more_badges() {
    // SAFETY: local paths are blocked for submodules by default; allow them in tests.
    unsafe { std::env::set_var("GIT_CONFIG_PARAMETERS", "'protocol.file.allow=always'") };
    let s = scratch("picker");
    let (server, r) = with_server(&s);
    // A worktree on a branch that is both ahead of and behind its remote branch.
    let other = s.root.join("other");
    git_ok(&s.root, &["clone", "-q", server.to_str().unwrap(), other.to_str().unwrap()]);
    git_ok(&other, &["config", "user.name", "Sam"]);
    git_ok(&other, &["config", "user.email", "sam@example.com"]);
    commit(&other, "x.txt", "x\n", "Theirs");
    git_ok(&other, &["push", "-q", "origin", "main:side"]);
    git_ok(&r, &["fetch", "-q"]);
    let wt = s.root.join("shop-side");
    git_ok(&r, &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap(), "main"]);
    git_ok(&wt, &["branch", "-q", "--set-upstream-to", "origin/side"]);
    commit(&wt, "w.txt", "w\n", "Worktree work");
    // A repository in the middle of a conflict, with one submodule.
    let c = new_repo(&s.root, "conflicted");
    commit(&c, "f.txt", "base\n", "Base");
    git_ok(&c, &["submodule", "-q", "add", r.to_str().unwrap(), "sub"]);
    git_ok(&c, &["commit", "-q", "-m", "Add sub"]);
    git_ok(&c, &["checkout", "-q", "-b", "b"]);
    commit(&c, "f.txt", "b\n", "B");
    git_ok(&c, &["checkout", "-q", "main"]);
    commit(&c, "f.txt", "main\n", "Main");
    assert!(git::run(&c, &["merge", "b"]).is_err());
    let mut h = app(&s, &[&r, &c], false);
    assert!(shows_containing(&h, "1 conflicts") && shows_containing(&h, "1 submodule"));
    assert!(shows_containing(&h, "↑1") && shows_containing(&h, "↓1"));
    // The worktree box's own Open button.
    let wt_box = h.get_by_label("Worktree side").rect();
    h.get_all_by_label("Open").find(|n| wt_box.contains(n.rect().center())).unwrap().click();
    settle(&mut h);
    assert!(shows(&h, "Close shop-side"));
    click(&mut h, "⌂  Overview");

    // What happens after the folder picker returns: several repositories, none, a destination.
    let nest = s.root.join("nest");
    let found: Vec<PathBuf> = ["p", "q"].iter().map(|n| {
        let d = new_repo(&nest, n);
        commit(&d, "f.txt", "f\n", n);
        d
    }).collect();
    inject(&mut h, super::Msg::Picked(nest.clone(), found));
    assert!(shows_containing(&h, "Added 2 repositories"));
    let empty = s.root.join("empty-folder");
    std::fs::create_dir_all(&empty).unwrap();
    inject(&mut h, super::Msg::Picked(empty.clone(), vec![]));
    assert!(shows(&h, "Create repository"));
    click(&mut h, "Cancel");
    inject(&mut h, super::Msg::PickClosed);
    click(&mut h, "+  Add");
    click(&mut h, "Clone from a web address…");
    inject(&mut h, super::Msg::PickedDest(s.root.join("dest")));
    assert!(h.query_all_by_value(&s.root.join("dest").display().to_string()).next().is_some());
    click(&mut h, "Cancel");

    // Odd requests: a commit that doesn't exist, a branch that isn't there.
    click(&mut h, "Card shop");
    double_click(&mut h, "Card shop");
    h.state_mut().do_action(super::Action::EditMessage("0000000000000000000000000000000000000000".into()));
    settle(&mut h);
    assert!(shows_containing(&h, "Couldn't read that commit's message"));
    h.state_mut().do_action(super::Action::SelectRef("no-such-branch".into()));
    settle(&mut h);
    assert!(shows_containing(&h, "not in the loaded history"));
    h.state_mut().do_action(super::Action::SelectCommit("0000000000000000000000000000000000000000".into()));
    settle(&mut h);

    // A new-worktree dialog takes a picked folder too.
    click(&mut h, "New worktree…");
    type_into(&mut h, "feature/my-change", "picked");
    inject(&mut h, super::Msg::PickedDest(s.root.join("elsewhere")));
    assert!(h.query_all_by_value(&s.root.join("elsewhere/shop-picked").display().to_string()).next().is_some());
    click(&mut h, "Cancel");

    // Removing a worktree whose tab is open closes that tab.
    click(&mut h, "Close shop");
    click(&mut h, "⌂  Overview");
    double_click(&mut h, "Card shop");
    // Three rows say "side": the branch, its worktree, and the remote branch (in that order).
    h.get_all_by_label("side").filter(|n| n.accesskit_node().role() == egui::Role::Button).nth(1).unwrap().click_secondary();
    settle(&mut h);
    click(&mut h, "Force remove…");
    click_last(&mut h, "Remove");
    assert!(!shows(&h, "Close shop-side"));
    assert!(!wt.exists());
}

// ---------------------------------------------------------------------------
// Merge conflicts

/// `main` and `feature` change lines 2, 10 and 18 of a 20-line a.txt and the one
/// line of b.txt differently, and the merge has stopped on all four conflicts.
fn conflicted_merge(s: &Scratch) -> (PathBuf, Vec<String>) {
    let r = new_repo(&s.root, "shop");
    let base: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
    write(&r, "b.txt", "colour\n");
    commit(&r, "a.txt", &base.concat(), "Base");
    let side = |who: &str| {
        let mut v = base.clone();
        for i in [1, 9, 17] {
            v[i] = format!("{who} {}\n", i + 1);
        }
        v.concat()
    };
    git_ok(&r, &["checkout", "-q", "-b", "feature"]);
    write(&r, "b.txt", "color\n");
    commit(&r, "a.txt", &side("feature"), "Feature edits");
    git_ok(&r, &["checkout", "-q", "main"]);
    write(&r, "b.txt", "colours\n");
    commit(&r, "a.txt", &side("main"), "Main edits");
    assert!(git::run(&r, &["merge", "feature"]).is_err(), "conflict");
    (r, base)
}

fn disabled(h: &Harness<'static, HeadwayApp>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

/// Click a clickable control with this name (not plain text that says the same).
fn click_control(h: &mut Harness<'static, HeadwayApp>, label: &str) {
    h.get_all_by_label(label)
        .find(|n| n.accesskit_node().role() != egui::Role::Label)
        .unwrap_or_else(|| panic!("no control “{label}”"))
        .click();
    settle(h);
}

fn alt(h: &mut Harness<'static, HeadwayApp>, k: egui::Key) {
    h.key_press_modifiers(egui::Modifiers::ALT, k);
    settle(h);
}

#[test]
fn walk_through_every_conflict_and_continue_the_merge() {
    let s = scratch("resolve");
    let (r, base) = conflicted_merge(&s);
    let mut h = app(&s, &[&r], true);

    // The banner says what's going on; Continue waits for the conflicts.
    assert!(shows(&h, "Merging feature into main · 2 conflicted files"));
    assert!(disabled(&h, "Continue"));

    // Clicking a conflicted file opens every conflict in the repository, in place of the history.
    click(&mut h, "a.txt");
    assert!(shows(&h, "Conflict 1 of 4"));
    assert!(shows(&h, "CONFLICTED FILES · 2"));
    assert!(shows(&h, "main (ours)") && shows(&h, "feature (theirs)"));
    assert!(!shows(&h, "Resize history and details"), "the history is out of the way");
    // Line numbers are the file's own: "main 2" sits on line 3, under the marker.
    assert!(shows(&h, "   3"));
    // The common ancestor shows on request.
    assert!(!shows(&h, "line 10"));
    click(&mut h, "Show base");
    assert!(shows(&h, "line 10"));
    click(&mut h, "Show base");

    // A choice moves on to the next conflict.
    click(&mut h, "Take main");
    assert!(shows(&h, "✓ took main") && shows(&h, "Conflict 2 of 4"));
    // Alt+↑/↓ move between conflicts; Alt+→ takes theirs.
    alt(&mut h, egui::Key::ArrowUp);
    assert!(shows(&h, "Conflict 1 of 4"));
    alt(&mut h, egui::Key::ArrowDown);
    alt(&mut h, egui::Key::ArrowRight);
    assert!(shows(&h, "✓ took feature") && shows(&h, "Conflict 3 of 4"));
    // The last region of a.txt done, on into b.txt.
    click(&mut h, "Both, main first");
    assert!(shows(&h, "Conflict 4 of 4") && shows(&h, "colours"));
    click(&mut h, "↑ Prev conflict");
    assert!(shows(&h, "Conflict 3 of 4"));
    click_last(&mut h, "Undo");
    click_last(&mut h, "Edit");
    click_text_containing(&mut h, "feature 18");
    ctrl(&mut h, egui::Key::A);
    type_text(&mut h, "both 18");
    click(&mut h, "Done");
    assert!(shows(&h, "Conflict 4 of 4"), "on to b.txt");
    alt(&mut h, egui::Key::ArrowLeft);
    assert!(shows(&h, "4 of 4 conflicts have a choice · 2 files ready to save"));

    // The whole result, before anything is written.
    click_control(&mut h, "a.txt");
    click(&mut h, "Preview result");
    assert!(shows(&h, "both 18") && !shows(&h, "<<<<<<< HEAD"));
    click(&mut h, "Preview result");
    assert!(std::fs::read_to_string(r.join("a.txt")).unwrap().contains("<<<<<<<"), "nothing written yet");

    click(&mut h, "Save & mark all ready (2)");
    let mut want = base.clone();
    want[1] = "main 2\n".into();
    want[9] = "feature 10\n".into();
    want[17] = "both 18\n".into();
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), want.concat());
    assert_eq!(std::fs::read_to_string(r.join("b.txt")).unwrap(), "colours\n");
    // b.txt is main's version again, so only a.txt differs from HEAD; neither is conflicted.
    assert_eq!(staged_files(&r), ["a.txt"]);
    assert!(git::status(&r).unwrap().conflicts.is_empty());
    // Nothing is left, so the resolver closes and Continue is ready.
    assert!(!shows(&h, "← Back to changes"));
    assert!(shows(&h, "Merging feature into main · all conflicts resolved"));
    assert!(!disabled(&h, "Continue"));
    click(&mut h, "Continue");
    assert!(!shows_containing(&h, "Merging feature"));
    assert_eq!(head_subject(&r), "Merge branch 'feature'");
    assert_eq!(git_ok(&r, &["rev-list", "--parents", "-1", "HEAD"]).split(' ').count(), 3);
}

#[test]
fn deleted_and_binary_conflicts_abort_and_skip() {
    let s = scratch("resolve-kinds");
    let r = new_repo(&s.root, "shop");
    commit(&r, "gone.txt", "1\n", "Base");
    commit(&r, "pic.bin", "\0base", "Picture");
    git_ok(&r, &["checkout", "-q", "-b", "other"]);
    git_ok(&r, &["rm", "-q", "gone.txt"]);
    commit(&r, "pic.bin", "\0theirs", "Other side");
    git_ok(&r, &["checkout", "-q", "main"]);
    write(&r, "gone.txt", "changed\n");
    commit(&r, "pic.bin", "\0ours", "Main side");
    assert!(git::run(&r, &["merge", "other"]).is_err());
    let mut h = app(&s, &[&r], true);
    assert!(shows(&h, "Merging other into main · 2 conflicted files"));

    // Delete/modify: what each side did, then keep the file or delete it.
    click(&mut h, "gone.txt");
    assert!(shows(&h, "other deleted the file") && shows(&h, "main's changes since the common ancestor"));
    assert!(shows(&h, "+ changed") && shows(&h, "− 1"));
    click(&mut h, "Keep the file (main's version)");
    // On to the next file: a binary one gets whole-file choices only.
    assert!(shows(&h, "Binary file · changed on both sides"));
    assert!(!shows(&h, "Take main"));
    click(&mut h, "Use other's version");
    assert!(shows(&h, "✓ Use other's version"));
    assert_eq!(std::fs::read_to_string(r.join("gone.txt")).unwrap(), "changed\n");
    click(&mut h, "Save & mark all ready (2)");
    assert_eq!(std::fs::read(r.join("pic.bin")).unwrap(), b"\0theirs");
    assert!(shows(&h, "Merging other into main · all conflicts resolved"));
    assert!(!shows(&h, "← Back to changes"), "the resolver closed");

    // Abort asks first, in the app, and says what's lost.
    click(&mut h, "Abort");
    assert!(shows_containing(&h, "Stop merging other into main?"));
    // Keeping gone.txt as it was changed nothing; the picture's resolution would be lost.
    assert!(shows_containing(&h, "your resolution of 1 file"));
    click_last(&mut h, "Abort merge");
    assert!(!shows_containing(&h, "Merging other"));
    assert_eq!(std::fs::read(r.join("pic.bin")).unwrap(), b"\0ours");

    // A rebase can skip the commit that stopped.
    git_ok(&r, &["checkout", "-q", "other"]);
    assert!(git::run(&r, &["rebase", "main"]).is_err());
    ctrl(&mut h, egui::Key::R);
    assert!(shows(&h, "Rebasing other onto main · commit 1 of 1 · 2 conflicted files"));
    click(&mut h, "Resolve conflicts");
    assert!(shows(&h, "Conflict 1 of 2"));
    assert!(disabled(&h, "Open in merge tool"), "no merge.tool is set");
    click(&mut h, "Skip");
    click_last(&mut h, "Skip commit");
    assert!(!shows_containing(&h, "Rebasing"));
    assert!(!shows(&h, "← Back to changes"));
    assert_eq!(git_ok(&r, &["rev-parse", "other"]), git_ok(&r, &["rev-parse", "main"]));
}

#[test]
fn one_file_at_a_time_in_a_narrow_window() {
    let s = scratch("resolve-narrow");
    let r = new_repo(&s.root, "shop");
    let base: Vec<String> = (1..=30).map(|i| format!("line {i}\n")).collect();
    let side = |who: &str| {
        let mut v = base.clone();
        v[1] = format!("{who} 2\n");
        v[24] = format!("{who} 25\n");
        v.concat()
    };
    write(&r, "src/b.txt", "keep\nold\n");
    write(&r, "d.txt", "one\ntwo\n");
    commit(&r, "a.txt", &base.concat(), "Base");
    git_ok(&r, &["checkout", "-q", "-b", "feature"]);
    write(&r, "src/b.txt", "keep\nnew\n");
    write(&r, "c.txt", "theirs\n");
    write(&r, "d.txt", "one\nTWO\n");
    commit(&r, "a.txt", &side("feature"), "Feature edits");
    git_ok(&r, &["checkout", "-q", "main"]);
    // main drops the line feature changed, adds its own c.txt and deletes d.txt.
    write(&r, "src/b.txt", "keep\n");
    write(&r, "c.txt", "ours\n");
    git_ok(&r, &["rm", "-q", "d.txt"]);
    commit(&r, "a.txt", &side("main"), "Main edits");
    assert!(git::run(&r, &["merge", "feature"]).is_err());
    let mut h = app_sized(&s, &[&r], true, egui::vec2(1000.0, 900.0));

    click(&mut h, "a.txt");
    assert!(shows(&h, "Conflict 1 of 5"));
    // A long unchanged stretch folds to its ends; a click shows it.
    assert!(!shows(&h, "line 12"));
    h.get_all_by_label_contains("unchanged lines").next().unwrap().click();
    settle(&mut h);
    assert!(shows(&h, "line 12"));

    // Choices survive a refresh.
    click(&mut h, "Take main");
    ctrl(&mut h, egui::Key::R);
    assert!(shows(&h, "✓ took main"));
    // A chosen region can be edited (or not) and undone.
    click(&mut h, "↑ Prev conflict");
    click(&mut h, "Edit");
    click(&mut h, "Cancel");
    click(&mut h, "Undo");
    // Marking resolved with conflicts left asks first.
    click(&mut h, "Save & mark resolved");
    assert!(shows_containing(&h, "2 conflicts in a.txt still have conflict markers"));
    click_last(&mut h, "Cancel");
    // Save writes the choices so far and leaves the file conflicted.
    click(&mut h, "Take main");
    click(&mut h, "Save");
    let text = std::fs::read_to_string(r.join("a.txt")).unwrap();
    assert!(text.contains("main 2\n") && text.contains("<<<<<<<"), "{text}");
    assert!(shows(&h, "1 of 1 left") && shows(&h, "Conflict 1 of 4"));
    // A whole side, straight from git; then it has no markers left to choose.
    click(&mut h, "Take all from feature");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), side("feature"));
    assert!(shows(&h, "no conflict markers left"));
    click(&mut h, "Save & mark resolved");
    // On to the next file: added on both sides, and deleting it can be undone.
    assert!(shows(&h, "added on both main and feature · 1 of 1 left"));
    click_control(&mut h, "Delete the file");
    assert!(shows(&h, "This file will be deleted."));
    click(&mut h, "Undo");
    click_control(&mut h, "Delete the file");
    // Deleted on main, changed on feature: what feature changed, stacked in a narrow window.
    click(&mut h, "↓ Next conflict");
    assert!(shows(&h, "main deleted the file") && shows(&h, "feature's changes since the common ancestor"));
    assert!(shows(&h, "+ TWO"));
    click(&mut h, "Keep the file (feature's version)");
    // src/b.txt: main's side of the region is empty.
    assert!(shows(&h, "(nothing)") && shows(&h, "src"));

    // Back to the history keeps every choice for later.
    click(&mut h, "← Back to changes");
    assert!(shows(&h, "Resize history and details"));
    click(&mut h, "Resolve conflicts");
    click_control(&mut h, "d.txt");
    assert!(shows(&h, "✓ Keep the file (feature's version)"));

    // The external merge tool, once merge.tool is set.
    git_ok(&r, &["config", "merge.tool", "takeTheirs"]);
    git_ok(&r, &["config", "mergetool.takeTheirs.cmd", "cp \"$REMOTE\" \"$MERGED\""]);
    git_ok(&r, &["config", "mergetool.takeTheirs.trustExitCode", "true"]);
    git_ok(&r, &["config", "mergetool.keepBackup", "false"]);
    ctrl(&mut h, egui::Key::R);
    click_control(&mut h, "b.txt");
    click(&mut h, "Open in merge tool");
    assert_eq!(std::fs::read_to_string(r.join("src/b.txt")).unwrap(), "keep\nnew\n");

    // The two remaining choices are saved together.
    assert!(shows(&h, "2 of 2 conflicts have a choice · 2 files ready to save"));
    click(&mut h, "Save & mark all ready (2)");
    assert!(git::status(&r).unwrap().conflicts.is_empty());
    assert!(!r.join("c.txt").exists());
    assert_eq!(std::fs::read_to_string(r.join("d.txt")).unwrap(), "one\nTWO\n");
    assert!(shows(&h, "Merging feature into main · all conflicts resolved"));
}

#[test]
fn a_conflicting_stash_pop_uses_the_names_git_gave_each_side() {
    let s = scratch("resolve-stash");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "start\n", "Start");
    write(&r, "a.txt", "stashed\n");
    git_ok(&r, &["stash", "-q"]);
    commit(&r, "a.txt", "committed\n", "Change");
    assert!(git::run(&r, &["stash", "pop"]).is_err());
    let mut h = app(&s, &[&r], true);
    // No merge, rebase, cherry-pick or revert: no banner, but the file can still be resolved.
    assert!(!shows(&h, "Continue"));
    click(&mut h, "a.txt");
    assert!(shows(&h, "Updated upstream (ours)") && shows(&h, "Stashed changes (theirs)"));
    click(&mut h, "Take Stashed changes");
    click(&mut h, "Save & mark all ready (1)");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "stashed\n");
    assert!(!shows(&h, "← Back to changes"));
}

// ---------------------------------------------------------------------------
// Undo and the reflog

#[test]
fn toast_undo_button_brings_discarded_lines_back() {
    let s = scratch("undo-toast");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\ntwo\n", "Start shop");
    let edited = "one\nNEW LINE\ntwo\n";
    write(&r, "a.txt", edited);
    let mut h = app(&s, &[&r], true);

    click(&mut h, "line NEW LINE");
    click(&mut h, "Discard lines");
    click_last(&mut h, "Discard");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), "one\ntwo\n");
    assert!(shows(&h, "Discard lines: done"));

    click(&mut h, "Undo");
    assert_eq!(std::fs::read_to_string(r.join("a.txt")).unwrap(), edited);
    assert!(shows(&h, "Undid discard 1 line in a.txt"));
    // Nothing left to undo: the shortcut says so instead of guessing.
    ctrl(&mut h, egui::Key::Z);
    assert!(shows(&h, "Nothing to undo"));
}

#[test]
fn undo_from_the_menu_and_with_the_shortcut() {
    let s = scratch("undo-menu");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\n", "Start shop");
    git_ok(&r, &["branch", "feature"]);
    write(&r, "b.txt", "two\n");
    git_ok(&r, &["add", "-A"]);
    let mut h = app(&s, &[&r], true);

    type_into(&mut h, "Describe your change", "Add cart page");
    click(&mut h, "Commit 1 file to main");
    assert_eq!(head_subject(&r), "Add cart page");

    // The repository menu says what Undo will take back.
    click(&mut h, "Repository menu");
    h.get_all_by_label_contains("Undo: commit “Add cart page”").next().expect("the menu names the commit").click();
    settle(&mut h);
    assert_eq!(head_subject(&r), "Start shop");
    assert_eq!(staged_files(&r), ["b.txt"], "the undone commit's changes are staged again");

    // Delete a branch, then Ctrl+Z.
    right_click(&mut h, "feature");
    click(&mut h, "Force delete…");
    click_last(&mut h, "Force delete");
    assert!(git::run(&r, &["rev-parse", "--verify", "-q", "feature"]).is_err());
    ctrl(&mut h, egui::Key::Z);
    assert_eq!(git_ok(&r, &["rev-parse", "feature"]), git_ok(&r, &["rev-parse", "main"]));
    assert!(shows(&h, "Undid delete branch feature"));
}

#[test]
fn reflog_finds_a_lost_commit_and_resets_to_it() {
    let s = scratch("reflog");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\n", "Start shop");
    commit(&r, "b.txt", "two\n", "Add cart page");
    git_ok(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    let mut h = app(&s, &[&r], true);
    assert!(!shows(&h, "Add cart page"), "the commit left the history");

    click(&mut h, "Reflog");
    assert!(shows_containing(&h, "reset: moving to HEAD~1"));
    h.get_all_by_label_contains("commit: Add cart page").next().expect("the lost commit is in the reflog").click_secondary();
    settle(&mut h);
    hover(&mut h, "Move main to this commit");
    click(&mut h, "Discard everything after it");
    assert!(shows_containing(&h, "main moves from"), "the confirmation names what moves");
    click_last(&mut h, "Reset");
    assert_eq!(head_subject(&r), "Add cart page");
    assert!(shows(&h, "Add cart page"));

    // And back again from the notification.
    click(&mut h, "Undo");
    assert_eq!(head_subject(&r), "Start shop");

    // Another branch's reflog, picked from the list.
    git_ok(&r, &["branch", "topic"]);
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Reflog of HEAD");
    click_last(&mut h, "topic");
    assert!(shows_containing(&h, "branch: Created from"));
}

fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[test]
fn changed_pictures_show_before_and_after() {
    let s = scratch("pictures");
    let r = new_repo(&s.root, "art");
    std::fs::write(r.join("logo.png"), png(40, 20, [200, 0, 0])).unwrap();
    commit(&r, "readme.txt", "hi\n", "Add logo");
    std::fs::write(r.join("logo.png"), png(64, 32, [0, 0, 200])).unwrap();
    write(&r, "icon.svg", r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="10"/></svg>"#);
    std::fs::write(r.join("broken.png"), b"not really a png").unwrap();
    let mut h = app(&s, &[&r], true);

    // Uncommitted: the edited picture side by side, the new ones on their own.
    assert!(shows(&h, "Before  ·  40 × 20"), "old picture");
    assert!(shows(&h, "After  ·  64 × 32"), "new picture");
    assert!(shows(&h, "Added  ·  24 × 24"), "new drawing");
    assert!(shows(&h, "Added"), "unreadable picture still gets a title");

    // A commit's changes show the picture it added.
    git_ok(&r, &["checkout", "--", "logo.png"]);
    std::fs::remove_file(r.join("broken.png")).unwrap();
    std::fs::remove_file(r.join("icon.svg")).unwrap();
    ctrl(&mut h, egui::Key::R);
    click(&mut h, "Add logo");
    assert!(shows(&h, "Added  ·  40 × 20"));
}

#[test]
fn conflicted_pictures_show_every_side() {
    let s = scratch("picture-conflicts");
    let r = new_repo(&s.root, "art");
    std::fs::write(r.join("logo.png"), png(40, 20, [200, 0, 0])).unwrap();
    std::fs::write(r.join("icon.png"), png(16, 16, [0, 200, 0])).unwrap();
    commit(&r, "readme.txt", "hi\n", "Base");
    git_ok(&r, &["checkout", "-q", "-b", "other"]);
    std::fs::write(r.join("logo.png"), png(64, 32, [0, 0, 200])).unwrap();
    std::fs::remove_file(r.join("icon.png")).unwrap();
    commit(&r, "readme.txt", "hi\n", "Other side");
    git_ok(&r, &["checkout", "-q", "main"]);
    std::fs::write(r.join("logo.png"), png(30, 30, [0, 200, 200])).unwrap();
    std::fs::write(r.join("icon.png"), png(24, 24, [200, 200, 0])).unwrap();
    commit(&r, "readme.txt", "hi\n", "Main side");
    assert!(git::run(&r, &["merge", "other"]).is_err());
    let mut h = app(&s, &[&r], true);

    // Changed on both sides: ours, theirs and the common ancestor, by name.
    click(&mut h, "logo.png");
    assert!(shows(&h, "main  ·  30 × 30"), "ours");
    assert!(shows(&h, "other  ·  64 × 32"), "theirs");
    assert!(shows(&h, "Common ancestor  ·  40 × 20"));
    click(&mut h, "Use other's version");
    // Deleted on one side: the side that deleted it says so.
    click(&mut h, "icon.png");
    assert!(shows(&h, "main  ·  24 × 24"));
    assert!(shows(&h, "Deleted here"));
    assert!(shows(&h, "Common ancestor  ·  16 × 16"));
}

// ---------------------------------------------------------------------------
// Blame and file history

/// A repository where f.txt was renamed to g.txt (and changed) along the way.
fn renamed_file_repo(s: &Scratch) -> PathBuf {
    let r = new_repo(&s.root, "notes");
    commit(&r, "f.txt", "one\ntwo\nthree\n", "Add f");
    git_ok(&r, &["mv", "f.txt", "g.txt"]);
    commit(&r, "g.txt", "one\nTWO\nthree\nfour\n", "Rename f to g");
    commit(&r, "src/other.txt", "x\n", "Unrelated change");
    r
}

fn file_view<'a>(h: &'a Harness<'static, HeadwayApp>, r: &Path) -> Option<&'a crate::fileview::FileView> {
    h.state().tabs.get(r).and_then(|rv| rv.file_view.as_ref())
}

fn rev_of(dir: &Path, rev: &str) -> String {
    git_ok(dir, &["rev-parse", rev]).trim().to_string()
}

#[test]
fn blame_from_commit_details_clicks_through_to_a_commit() {
    use crate::fileview::{HistoryKind, Mode};
    let s = scratch("blame");
    let r = renamed_file_repo(&s);
    let (add_f, rename) = (rev_of(&r, "HEAD~2"), rev_of(&r, "HEAD~1"));
    let mut h = app(&s, &[&r], true);

    // Right-click a file in a commit's details.
    click(&mut h, "Rename f to g");
    right_click_row(&mut h, "g.txt");
    click(&mut h, "Blame");
    let fv = file_view(&h, &r).expect("blame is open");
    let b = fv.blame.as_ref().unwrap();
    assert_eq!((fv.mode, b.rev.as_deref(), b.path.as_str()), (Mode::Blame, Some(rename.as_str()), "g.txt"));
    assert!(b.done && b.error.is_none());
    assert!(b.owners.iter().all(|o| o.is_some()), "every line is blamed");
    assert_eq!(b.owner(0).unwrap().0.summary, "Add f");
    assert_eq!(b.owner(1).unwrap().0.summary, "Rename f to g");
    assert!(shows_containing(&h, "Newer"), "the age scale is shown");

    // Clicking the gutter shows that commit in the history.
    h.get_all_by_label_contains(&format!("{} Add f", &add_f[..8])).next().expect("a gutter entry for Add f").click();
    settle(&mut h);
    let rv = &h.state().tabs[&r];
    assert!(rv.selection == Some(super::Selection::Commit(add_f.clone())), "the commit is selected");
    assert!(!file_view(&h, &r).unwrap().shown);
    assert!(shows(&h, "✎ Edit message"), "the commit's details are on screen");
    click(&mut h, "Back to blame");
    assert!(file_view(&h, &r).unwrap().shown);

    // Blame the parent of a line: the old name, at the commit before.
    right_click(&mut h, "line 2");
    h.get_all_by_label_contains("Blame the parent").next().expect("Blame the parent in the menu").click();
    settle(&mut h);
    let fv = file_view(&h, &r).unwrap();
    let b = fv.blame.as_ref().unwrap();
    assert_eq!((b.rev.as_deref(), b.path.as_str()), (Some(add_f.as_str()), "f.txt"));
    assert_eq!(b.picked(), Some((1, 1)), "the same line stays picked");
    assert_eq!(fv.back.len(), 1);
    click(&mut h, "← Back");
    let b = file_view(&h, &r).unwrap().blame.as_ref().unwrap();
    assert_eq!((b.rev.as_deref(), b.path.as_str()), (Some(rename.as_str()), "g.txt"));

    // History of one line, from the keyboard.
    click(&mut h, "line 2");
    key(&mut h, egui::Key::L);
    let fv = file_view(&h, &r).unwrap();
    let hist = fv.history.as_ref().unwrap();
    assert_eq!((fv.mode, &hist.kind), (Mode::History, &HistoryKind::Lines { rev: Some(rename.clone()), start: 2, end: 2 }));
    let seen: Vec<&str> = hist.commits().iter().map(|(c, _)| c.summary.as_str()).collect();
    assert_eq!(seen, ["Rename f to g", "Add f"]);
    assert!(hist.diff.as_ref().is_some_and(|d| d.is_ok()), "the first commit's change is shown");

    // B goes back to the blame, Esc closes.
    key(&mut h, egui::Key::B);
    assert_eq!(file_view(&h, &r).unwrap().mode, Mode::Blame);
    key(&mut h, egui::Key::Escape);
    key(&mut h, egui::Key::Escape);
    assert!(file_view(&h, &r).is_none());
    assert!(shows(&h, "Unrelated change"), "back to the history");

    // Lines picked in a commit's diff have a history too.
    click(&mut h, "Rename f to g");
    click(&mut h, "line TWO");
    click(&mut h, "History of this line");
    let hist = file_view(&h, &r).unwrap().history.as_ref().unwrap();
    assert_eq!((&hist.kind, hist.path.as_str()), (&HistoryKind::Lines { rev: Some(rename.clone()), start: 2, end: 2 }, "g.txt"));
    assert_eq!(hist.commits().len(), 2);
}

#[test]
fn file_history_follows_a_rename_and_blames_the_working_copy() {
    use crate::fileview::{HistoryKind, Mode};
    let s = scratch("file-history");
    let r = renamed_file_repo(&s);
    write(&r, "g.txt", "one\nTWO\nthree\nfour\nfive\n");
    let mut h = app(&s, &[&r], true);

    // Blame the working copy from the changes list: the new line isn't committed.
    right_click_row(&mut h, "g.txt");
    click(&mut h, "Blame");
    let b = file_view(&h, &r).unwrap().blame.as_ref().unwrap();
    assert_eq!(b.rev, None);
    assert!(b.owner(4).unwrap().0.uncommitted());
    assert!(shows(&h, "Not committed yet") || shows_containing(&h, "uncommitted"));

    // Find a file with Ctrl/Cmd+P; Ctrl/Cmd+Enter opens its history.
    ctrl(&mut h, egui::Key::P);
    assert!(shows(&h, "Find file"));
    type_text(&mut h, "g.txt");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    settle(&mut h);
    let fv = file_view(&h, &r).unwrap();
    let hist = fv.history.as_ref().unwrap();
    assert_eq!((fv.mode, &hist.kind), (Mode::History, &HistoryKind::File));
    let seen: Vec<(&str, &str)> = hist.commits().iter().map(|(c, _)| (c.summary.as_str(), c.path.as_str())).collect();
    assert_eq!(seen, [("Rename f to g", "g.txt"), ("Add f", "f.txt")]);
    assert_eq!(hist.commits()[0].0.old_path.as_deref(), Some("f.txt"));

    // Picking the older commit shows its change to just this file.
    click(&mut h, "Add f");
    settle(&mut h);
    let hist = file_view(&h, &r).unwrap().history.as_ref().unwrap();
    assert_eq!(hist.selected, Some(1));
    let doc = hist.diff.as_ref().unwrap().as_ref().unwrap();
    assert_eq!(doc.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["f.txt"]);
    // …and blaming it there opens the file under its old name.
    click(&mut h, "Blame at this commit");
    let b = file_view(&h, &r).unwrap().blame.as_ref().unwrap();
    assert_eq!((b.path.as_str(), b.rev.clone()), ("f.txt", Some(rev_of(&r, "HEAD~2"))));

    // A folder's history, from Find file.
    ctrl(&mut h, egui::Key::P);
    type_text(&mut h, "src");
    key(&mut h, egui::Key::Enter);
    let fv = file_view(&h, &r).unwrap();
    let hist = fv.history.as_ref().unwrap();
    assert_eq!((&hist.kind, hist.path.as_str()), (&HistoryKind::Folder, "src"));
    assert_eq!(hist.commits().len(), 1);
    assert!(fv.blame.is_none());
}

#[test]
fn arrow_keys_in_find_file_leave_the_history_alone() {
    let s = scratch("find-file-keys");
    let r = renamed_file_repo(&s);
    let mut h = app(&s, &[&r], true);
    let selected = |h: &Harness<'static, HeadwayApp>| h.state().tabs[&r].selection.clone();
    let before = selected(&h);
    assert!(before == Some(super::Selection::Commit(rev_of(&r, "HEAD"))));

    ctrl(&mut h, egui::Key::P);
    key(&mut h, egui::Key::ArrowDown);
    assert!(selected(&h) == before, "the history selection didn't move");
    let Some(super::Dialog::FindFile { picked, hits, .. }) = &h.state().dialog else { panic!("Find file is still open") };
    assert_eq!((*picked, hits.len()), (1, 2), "the dialog's list moved instead");
    // Even in a frame where the text box has lost the keyboard (egui moves focus on
    // arrow keys in the real window), the history stays put while the dialog is open.
    h.ctx.memory_mut(|m| m.stop_text_input());
    key(&mut h, egui::Key::ArrowDown);
    assert!(selected(&h) == before);
    key(&mut h, egui::Key::ArrowUp);
    assert!(selected(&h) == before);
    // The text box keeps the keyboard: typing still filters.
    type_text(&mut h, "g.txt");
    let Some(super::Dialog::FindFile { hits, .. }) = &h.state().dialog else { panic!() };
    assert_eq!(hits[0].0, "g.txt");
}

#[test]
fn a_narrow_history_shows_two_lines_per_commit() {
    let s = scratch("two-line-history");
    let r = new_repo(&s.root, "shop");
    commit(&r, "a.txt", "one\n", "Start shop");
    commit(&r, "b.txt", "two\n", "Add cart page");
    let row_height = |h: &Harness<'static, HeadwayApp>| {
        h.get_all_by_label("Start shop").find(|n| n.accesskit_node().role() == egui::Role::Row).expect("a history row").rect().height()
    };

    // A wide list: one line per commit, in columns.
    let h = app(&s, &[&r], true);
    assert_eq!(row_height(&h), 26.0);

    // A narrow one: the subject, then author, time and commit id below it.
    let mut h = app_sized(&s, &[&r], true, egui::vec2(1000.0, 800.0));
    assert_eq!(row_height(&h), 44.0);
    // Still a normal list: clicking and arrow keys pick commits.
    click(&mut h, "Start shop");
    assert!(shows(&h, "✎ Edit message"));
    key(&mut h, egui::Key::ArrowUp);
    assert!(h.state().tabs[&r].selection == Some(super::Selection::Commit(rev_of(&r, "HEAD"))));
}

#[test]
fn ignore_whitespace_hides_reindented_files_and_stops_line_staging() {
    let s = scratch("ignore-ws");
    let r = new_repo(&s.root, "tidy");
    commit(&r, "a.rs", "fn a() {\n    x();\n}\n", "Start");
    commit(&r, "b.rs", "fn b() {}\n", "Add b");
    write(&r, "a.rs", "fn a() {\n\tx();\n}\n");
    write(&r, "b.rs", "fn b() { y(); }\n");
    let mut h = app(&s, &[&r], true);
    assert_eq!(h.get_all_by_label("Stage hunk").count(), 2, "both files have a block of changes");

    click(&mut h, "Ignore whitespace");
    assert!(h.state().cfg.view.diff.ignore_whitespace, "remembered in the settings");
    assert!(shows(&h, git::WHITESPACE_ONLY), "the re-indented file says why it shows nothing");
    assert!(!shows(&h, "Stage hunk"), "a -w diff doesn't match the file, so it can't be staged from");
    assert!(shows_containing(&h, "Whitespace changes are hidden"));

    // A whole file can still be staged.
    click_in_row(&mut h, "b.rs", "Stage");
    assert_eq!(staged_files(&r), ["b.rs"]);

    click(&mut h, "Ignore whitespace");
    assert!(!shows(&h, git::WHITESPACE_ONLY));
    assert_eq!(h.get_all_by_label("Stage hunk").count(), 1, "a.rs's block can be staged again");
}
