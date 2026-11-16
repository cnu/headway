# Forge fixtures

Answers from `gh` and `glab`, read by the parsing tests. Tests never call the network.

- `gh-*.json`: recorded from `gh api` on the public `cli/cli` repository
  (2026-11-11), with the query in `github.rs`. People's logins and fork owners
  were replaced with `user-N`; bots kept their names. `gh-user.json` keeps only
  the fields Headway reads. `gh-prs-for-branches.branches` lists the branches
  asked about, in order (`b0`, `b1`, …).
- `gh-pr-lists.json`: the sidebar's lists query (`lists_query(5)`), recorded the
  same day. Every login, bots included, is `user-N`. The recording account had
  nothing in its own two lists, so `mine` and `review` hold a copy of the first
  and second open pull request, by and asking `octo-user` (the user in
  `gh-user.json`).
- `glab-*.json`: written by hand from GitLab's GraphQL and REST documentation,
  in the shape `glab api` prints, since `glab` wasn't available to record from.
  Replace them with recordings when it is.
- `glab-mr-lists.json`: the sidebar's lists query, put together from the open
  merge requests in `glab-mrs-for-branches.json` plus one by `tanuki-user`.
- `gh-pr-details.json`, `gh-pr-checks.json`, `gh-pr-conversation.json`: a pull
  request's view (`details_query`, `checks_query`, `conversation_query` in
  `github.rs`), written by hand in the shape of GitHub's GraphQL schema: a
  failing Actions job, one still running, a neutral app check and a commit
  status; a review asking for changes with a line comment, a comments-only
  review and an unsubmitted one. The description exercises the markdown view.
- `gh-run-log-failed.txt`: what `gh run view <run> --log-failed` prints
  (`job<TAB>step<TAB>time text`), 340 lines of one job plus a line of another.
- `glab-mr-details.json`, `glab-mr-checks.json`, `glab-mr-conversation.json`:
  the same for a merge request, by hand from GitLab's GraphQL documentation:
  jobs allowed to fail, a manual one, a running one; notes on the diff and
  GitLab's own notes.
