# Forge fixtures

Answers from `gh` and `glab`, read by the parsing tests. Tests never call the network.

- `gh-*.json`: recorded from `gh api` on the public `cli/cli` repository
  (2026-11-11), with the query in `github.rs`. People's logins and fork owners
  were replaced with `user-N`; bots kept their names. `gh-user.json` keeps only
  the fields Headway reads. `gh-prs-for-branches.branches` lists the branches
  asked about, in order (`b0`, `b1`, …).
- `glab-*.json`: written by hand from GitLab's GraphQL and REST documentation,
  in the shape `glab api` prints, since `glab` wasn't available to record from.
  Replace them with recordings when it is.
