# Changelog

All notable changes to Headway are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Headway uses
[semantic versioning](https://semver.org/).

When releasing, rename **Unreleased** to the new version and date. The release
workflow uses that version's section as the GitHub release notes.

## [Unreleased]

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
