# Continuous integration

Woodpecker, on the project's own server. Pipelines live in `.woodpecker/`.

| Pipeline | When | What |
|---|---|---|
| `ci.yaml` | every push and pull request | `cargo fmt --check`, `cargo clippy` (errors fail; about 220 style warnings are being worked down), `cargo test --workspace --locked` |
| `publish.yaml` | a push to `main` (a merged pull request is one), and `v*` tags; after `ci.yaml` passes | publishes to the public GitHub repository: see below |
| `audit.yaml` | `Cargo.lock` or `.cargo/audit.toml` changes, and a cron job named `audit` (set it up in the repo's Woodpecker settings, daily or weekly) | `cargo audit` against RustSec |

Run the same checks locally before pushing: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked && cargo test --workspace --locked`.

**Formatting** is rustfmt with `rustfmt.toml` (width 120, "Max" heuristics, which suits this code's dense style). It was applied in one commit,
which `.git-blame-ignore-revs` hides from `git blame` (`git config blame.ignoreRevsFile .git-blame-ignore-revs`).

**Audit ignores** are in `.cargo/audit.toml`, each with the reason and what would remove it: libtorch's zip (opt-in feature only) and the Linux accessibility bridge's XML parser (egui 0.29). Everything else fails the build. When egui is
upgraded, remove the matching ignores.

**Not covered here yet:** Windows and macOS (CI proves nothing there; `cargo xwin check --target x86_64-pc-windows-msvc -p dubplate` is run by hand), the
accuracy check on real tracks (`barscore`, needs audio the CI cannot reach).

## Publishing to GitHub

Work happens here, on `private` and branches off it. Merging into `main` publishes `main` to the public GitHub
repository as **one new commit** on top of what GitHub has, written by the public identity, with the message
`Dubplate <version> (<date>)`. Only the files go: none of this repository's commits, messages, authors, dates or
branches. Paths marked `export-ignore` in `.gitattributes` are left out. GitHub's own workflows (`.github/`) then build on
Linux, Windows and macOS, and a `v*` tag builds release packages.

Before pushing, `scripts/publish_github.sh` searches every file, binary files included, and the commit message. It
looks for the private words in the `publish_deny` secret, home folders other than made-up ones (`/home/dj`,
`/Users/me`), `/mnt/…/` paths, Claude's scratch folders, and private network addresses. One hit stops the publish, and
the log says which file. The fix is to remove the thing from the files and merge again: history here does not matter,
since it is never published.

To release, tag a commit on `main` with `vX.Y.Z` and push the tag. The tag goes on the published commit, and GitHub
builds the packages.

**Setting it up (once):**

1. Create the GitHub repository under the public account, empty (no README or licence: the first publish adds them).
2. Make a deploy key: `ssh-keygen -t ed25519 -N '' -C dubplate-publish -f publish_key`. In the GitHub repository go to
   Settings → Deploy keys → Add, paste `publish_key.pub`, and tick *Allow write access*.
3. In this repository's Woodpecker settings, under Secrets, add the following, each allowed for the *push* and *tag*
   events only:
   `github_repo` (`git@github.com:<account>/dubplate.git`), `github_deploy_key` (the contents of `publish_key`),
   `publish_name`, `publish_email` (the account's `…@users.noreply.github.com` address), and `publish_deny`, one
   word per line: your real email and its domain, user names, machine names, the server's address. Then delete
   `publish_key` from disk.
4. Try it without pushing anything (the environment variables are in `scripts/publish_github.sh`):
   `PUBLISH_DRY_RUN=1 PUBLISH_REPO=… PUBLISH_NAME=… PUBLISH_EMAIL=… PUBLISH_DENY=$'…' scripts/publish_github.sh`.

The publish starts from GitHub's current `main` each time and never forces. A change made directly on GitHub (a merged
outside pull request, say) is overwritten by the next publish, though it stays in GitHub's history, so bring such
changes back here first.
