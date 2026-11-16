# Changelog

All notable changes to Headway are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Headway uses
[semantic versioning](https://semver.org/).

When releasing, rename **Unreleased** to the new version and date. The release
workflow uses that version's section as the GitHub release notes.

## [Unreleased]

### Added

- **Interactive rebase**: right-click a commit and choose *Interactively
  rebase from here…* to edit the commits from it up to HEAD. Each row has an
  action (pick, reword, edit, squash, fixup, drop); drag rows or press
  Alt+↑/↓ to reorder them, and P R E S F D to set the action. Squash and
  fixup rows sit under the row above, rewords and squashed commits have their
  message editable in place, and a live count says "5 commits → 3". It runs
  `git rebase -i --update-refs` (with `--autostash` when you have uncommitted
  changes), so branches pointing into the range move along; branches checked
  out in another worktree stay put. If it stops for an `edit` step or a
  conflict, the banner says "Stopped at 3 of 7" with Continue, Skip and Abort,
  and the whole rebase is one entry in Undo. `fixup!` and `squash!` commits in
  the range can be placed under their commits with one button. Ranges that
  contain merge commits are refused with an explanation. git runs Headway
  itself as its editor for this, through the hidden `--write-rebase-todo` and
  `--write-rebase-message` options.
- **Fixup into…** in the commit box: pick an earlier commit, and the staged
  changes are committed as a fixup and folded into it in one step. Aborting a
  conflict puts the changes back in the staging area.
- A *rebase-lab* sample project (development builds) with `fixup!` commits, a
  WIP commit, a conflicting pair and a branch in the middle of its history.

### Changed

- The banner for a rebase that stopped says "Stopped at 3 of 7", counting only
  commits (not the `update-ref` and `drop` lines in git's list).

## [0.3.0] - 2026-11-16

### Added

- **Review inbox and alerts**: the workspace overview has an Inbox tab next to
  the repository cards, listing the pull requests (and merge requests) that
  need you across every repository in the workspace: reviews asked of you or
  your team, then yours with failing checks, with changes requested, ready to
  merge, and your other open ones, each with its repository, number, title,
  author, checks, review state and age. Click one to open it in its
  repository's window. Pin one to the top, or snooze it until tomorrow or next
  week (kept with the workspace); the tab counts what needs doing. Each GitHub
  or GitLab host is asked once for the whole workspace (GitHub's searches,
  GitLab's lists of yours), on the usual refresh schedule and rate limits, and
  only for the workspace that's open. When something changes for you (checks
  fail on yours, yours is approved, changes are requested, someone asks for
  your review, yours is merged), a notification says so with a button for
  what's next (Show checks, Open, Delete branch), and the window asks for
  attention when it's in the background. Each alerts once, and nothing from
  before Headway started; each kind can be turned off in the Inbox's Alerts
  menu. Repositories whose forge isn't connected are listed with how to fix it.
- **GitHub and GitLab connection**: Headway finds out which repositories are on
  GitHub or GitLab (GitHub Enterprise and self-managed GitLab too) and talks to
  them through the `gh` and `glab` you're already signed in to, so it never
  asks for or stores a token. Each repository's ⋯ menu says who you're signed
  in as, or exactly what to install or run to connect. Pull request status
  stays up to date in the background. Every `gh` and `glab` call is in the
  command log, with tokens taken out. Without either CLI, nothing changes.
- **Pull request and check status**: branches with a pull request on GitHub or
  GitLab carry its number and state (open, draft, merged or closed, each with
  its own shape) and a check mark (passing, failing or running) in the
  sidebar and the history; the toolbar says it in words ("#13 · checks failing
  · changes requested"), and the overview card shows the current branch's,
  filled red when a check fails. Hover for the title, author, reviews, check
  counts and how fresh it is; click to open it. Branches
  are matched by their upstream, forks included.
- **Merged branches**: a branch whose pull request was merged says so, and its
  menu offers "Delete branch (merged in #123)", undoable as always. The
  overview counts them per repository; click the count to see just those.
- **Pull requests in the sidebar** ("Merge requests" on GitLab): yours, the
  ones waiting for your review, and every open one, each with its author,
  checks, review state, draft marker and age, and the full details on hover.
  Right-click (or ⋯) to check one out on a local branch named after it (or
  `pr/123` when that name is taken), fetched with git so it's in the command
  log and undoable; to check it out in a new worktree next to the repository,
  to review it without touching your work; to show its commits in the
  history; to add the fork it comes from as a remote; or to open it, copy its
  link or its branch name. Arrows and Enter work in the list.
- **Create pull requests** ("merge requests" on GitLab) without leaving
  Headway: from a branch's right-click menu, the "New pull request…" row in
  the sidebar, or the button on the notification after pushing a branch that
  has none. The title is the commit's subject (or the branch name in words),
  the description the repository's template (pick one when there are several;
  read from the base branch) or the list of commits. Pick another base, mark
  it a draft, and choose reviewers, assignees and labels from the forge's
  lists; on GitLab, delete the source branch when merged and squash. It says
  what goes in ("3 commits, 7 files changed, into upstream/main"), pushes the
  branch first when the remote is missing commits (a rejected push stops
  there, with git's message), and warns when it will conflict with the base,
  naming the files (git 2.38 or later). For a fork, it goes from your copy into
  upstream. The new pull request's badge shows straight away, and the
  notification opens it in the browser.
- **Pull requests in the window** ("merge requests" on GitLab): clicking a
  badge or a row in the sidebar opens the pull request over the history (Esc
  closes it; "Open on GitHub" is still a button). Its header has the title,
  state, author, `base ← head`, draft and review state, and Check out. The
  description and the conversation (comments, reviews, and line comments with
  their file and line) are shown as formatted markdown; the commits each open
  their diff; the files changed are the whole base-to-head diff in the usual
  diff view, side by side and coloured, with the head fetched first if it
  isn't here. Checks are grouped failing first, with how long each took and a
  link; a failed GitHub Actions job or GitLab job shows the last 300 lines of
  its log. "Re-run failed" starts the failed Actions runs (or retries the
  GitLab pipeline) again, and says why when it can't. Each part loads on its
  own, so a slow log never holds up the description, and the checks are read
  again every 30 seconds while any is running.
- **Review and merge pull requests**: a comment box under the conversation
  (with a preview) posts a comment, or approves or requests changes on
  someone else's pull request (on GitLab: approve or revoke your approval).
  What you've written stays with each pull request until it's sent. Pick
  lines in Files changed to comment on them: on GitHub they collect into one
  review sent with Submit review; on GitLab each is posted as a discussion on
  the diff. Mark a draft ready for review or turn it back into a draft, and
  close (after asking) or reopen it. **Merge** offers only the methods the
  repository allows (merge commit, squash or rebase on GitHub; merge or
  fast-forward and squash on GitLab), remembers the last one per repository,
  fills in a squash commit's title and message the way GitHub does, and can
  delete the branch on the forge and the local one (undoably). What stands in
  the way (failing or running checks, a review needed, conflicts, a branch
  behind its base) is listed, and Merge says why it can't; when GitHub's
  auto-merge can wait for it instead, "Merge when checks pass" turns that on.
  Merging asks once more, naming the method and the base, since Headway can't
  undo it; afterwards Headway fetches, and if you were on the merged branch,
  offers to switch to the base and pull. A refusal from GitHub or GitLab is
  shown in its own words, and every action is in the command log.
- `headway --version` (or `-V`) prints the version, and `--help` lists every
  option with what it does.

### Removed

- `--screenshot` is only in development builds now.

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

[Unreleased]: https://github.com/cnu/headway/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/cnu/headway/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/cnu/headway/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/cnu/headway/releases/tag/v0.1.0
