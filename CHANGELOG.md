# Changelog

All notable changes to Headway are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Headway uses
[semantic versioning](https://semver.org/).

When releasing, rename **Unreleased** to the new version and date. The release
workflow uses that version's section as the GitHub release notes.

## [Unreleased]

### Added

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

[Unreleased]: https://github.com/cnu/headway/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/cnu/headway/releases/tag/v0.1.0
