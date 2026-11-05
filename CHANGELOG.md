# Changelog

All notable changes to Headway are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Headway uses
[semantic versioning](https://semver.org/).

When releasing, rename **Unreleased** to the new version and date. The release
workflow uses that version's section as the GitHub release notes.

## [Unreleased]

## [0.2.0] - 2026-11-05

### Added

- **Command log**: every git command Headway runs is recorded on your
  computer, with the folder, the action you took that ran it, how long it
  took and what git said, so there's a clear record if something goes wrong.
  Commands that change something and every action (with the undo entry it
  left, and the branches and tags it moved) are kept for 14 days in
  `logs/` under the data folder; read-only commands are only kept in memory
  unless `HEADWAY_LOG=verbose`. Passwords and tokens are taken out, and
  nothing is sent anywhere. `headway --export-log report.jsonl` saves the
  last two days to attach to a bug report, `headway --log-path` says where
  the log is, and `HEADWAY_LOG=off` turns it off.
- **Syntax highlighting** in diffs and blame, for about 200 languages. Each
  side of a change is coloured as a whole file, so strings and comments that
  span lines come out right; it happens in the background once the diff is on
  screen, and each version of a file is only coloured once.
- **Changed words**: when a line was edited rather than rewritten, the exact
  words that changed stand out on both the removed and the added line. A
  change to only spaces or tabs shows them as · and →. **Ignore whitespace**
  (above the diff, remembered) leaves such changes out with `-w`; files with
  nothing else changed say so, and lines and hunks can't be staged until it's
  off again.
- **Side by side**: show a diff as before and after in two columns (remembered
  with the other diff options). Edited lines face their new version, added and
  removed lines face a gap, and both columns scroll together. Pick, stage,
  unstage and discard lines and hunks just as in the unified view; picks stay
  put when switching between the two.
- **Comparing pictures**: an edited picture's titles give its size in pixels
  and in bytes, and besides **2-up** (next to each other) it can be shown as a
  **Swipe** (move a line across to uncover before or after) or an **Onion
  skin** (after fades in over before).
- **Binary files** say how big they were and are ("Binary file, 2.1 MB →
  2.3 MB") instead of only "Binary file".
- **Large changes**: a file with more than 300 changed lines is held back with
  a **Show anyway** button, in commits as well as uncommitted changes, so a
  huge generated file doesn't slow the diff down. Showing one keeps it shown
  while you stage. Development builds add a **diff-showcase** sample with one
  of everything (`--reset-sample diff-showcase` builds it in an existing
  setup).
- **Blame and file history**: right-click a file in a commit's details, the
  changes list or a diff, or find any file or folder with Ctrl/Cmd+P. Blame
  shows who last changed each line, grouped by commit, with an age colour
  (newer is stronger); lines appear as git finds them, so big files show at
  once. Hover for the commit's summary, click to jump to it in the history.
  **Blame the parent** (P) steps back past a formatting or moving change, and
  Back (Alt+←) returns. Ignore whitespace, look past moved or copied lines,
  and skip the commits in `blame.ignoreRevsFile` or `.git-blame-ignore-revs`.
  **History** lists the commits that changed a file (following renames) or a
  folder, with just that file's or folder's changes. Pick lines in blame or a
  commit's diff for **History of these lines** (L). B and H switch views, Esc
  closes. Development builds' **api-server** sample has a moved file and a
  reformat that blame skips.
- **Narrow history**: when the history pane is narrower than its columns
  need, each commit takes two lines: the subject (with its branch and tag
  labels), then the author, time and commit id.
- **Image previews**: changed pictures (PNG, JPEG, GIF, WebP, BMP, ICO and
  SVG) show in the diff instead of "Binary file", before and after side by
  side, with a checkerboard behind transparent parts. SVGs also keep their
  text diff. The conflict resolver shows a conflicted picture as each branch
  has it, next to the common ancestor.
- **Merge conflicts**: a banner shows the merge, rebase, cherry-pick or revert
  in progress (with Continue, Abort and, for sequences, Skip; Abort asks first
  and says what will be lost). The resolver walks through every conflict in
  the repository one at a time (Prev/Next, Alt+↑/↓), with each file's progress
  in a list: take one side (Alt+←/→), both in either order, or edit the result,
  labelled with the real branch or commit names. Line numbers, the common
  ancestor on request, ours and theirs side by side on wide windows, and a
  preview of the whole result. Choices are kept until you save; save one file
  or every ready file at once. Delete/modify conflicts show what the changing
  side did before you keep or delete the file; binary files get whole-file
  choices. Take a whole side, or open your `merge.tool`. diff3 and zdiff3
  markers work.
  Development builds add a **price-rules** sample stopped in a conflicted merge.
- **Undo**: commits, amends, message edits, resets, merges, rebases,
  cherry-picks, reverts, pulls, checkouts, branch and tag deletes, stash drops
  and discarded files, hunks or lines can be undone with Ctrl/Cmd+Z, the
  repository's ⋯ menu ("Undo: discard hunk in src/app.rs") or the Undo button
  on the notification. Discarded work is saved in git first, byte for byte,
  new files included. Undo refuses, and says why, when it isn't safe: the
  branch has moved or been pushed since, newer actions depend on it, or your
  uncommitted changes would be overwritten. The last 100 actions per
  repository are kept, across restarts.
- **Reflog**: a sidebar item lists everywhere HEAD or a chosen branch has been,
  with the action and when. Right-click an entry to create a branch there,
  check it out, or move the current branch to it (the confirmation says what
  moves from where). Development builds' **web-shop** sample has a commit only
  the reflog can find.

## [0.1.0] - 2026-11-04

First release.

### Added

- **Workspaces**: named groups of repositories with an overview of each one's
  branch, uncommitted changes and ahead/behind counts; fetch or pull them all;
  open tabs are remembered. Add a repository, every repository in a folder, or
  clone from a web address.
- **History**: branch graph with refs, author, short commit id and date;
  search (Ctrl+F); commit details with full diffs; Up/Down, Page Up/Down and
  Home/End navigation. Hide branches from the history with the eye icon on
  each branch, remembered per repository.
- **Staging**: every changed file stacked in Unstaged and Staged groups; stage,
  unstage or discard whole files, hunks or individual lines; fold files, or
  collapse and expand them all.
- **Commits**: commit, amend, and edit the message of any earlier commit; later
  commits and every branch, tag, worktree and detached HEAD on them move along
  in one step.
- **Branches, tags and stashes** in the sidebar, with checkout, merge, rebase,
  cherry-pick, revert and reset.
- **Remotes**: add a remote (and fetch it straight away), fetch one, copy its
  address or remove it.
- **Worktrees**: create, open, remove and clean up worktrees; branches checked
  out in another worktree are marked; the overview shows each repository's
  worktrees inside its card, scrolling when there are many.
- **Submodules**: see each one's state; set up, update, reset, open or remove
  them; add a submodule; cloning fetches submodules too.
- App icon, Linux desktop entry and install script; README.
- Downloads for macOS (`.dmg`, Apple silicon and Intel), Windows (`.msi`,
  `.zip`) and Linux (`.deb`, `.rpm`, AppImage and `.tar.gz`, x86_64 and arm64);
  a Homebrew cask (`brew install --cask cnu/tap/headway`) and an AUR package
  (`headway-bin`).

[Unreleased]: https://github.com/cnu/headway/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/cnu/headway/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/cnu/headway/releases/tag/v0.1.0
