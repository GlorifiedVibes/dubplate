#!/usr/bin/env bash
# Publishes the tree of the current commit to the public GitHub repository as ONE new commit on top of what is there,
# written by the public identity. Nothing of the private history goes: no commits, messages, authors or dates, only
# the files (as `git archive` gives them, so paths marked export-ignore in .gitattributes are left out).
#
# Before anything is pushed, every file and the commit message are searched for things that must stay private (see
# `scan` below); one hit stops the publish. Run by .woodpecker/publish.yaml; can be run by hand the same way.
#
# Environment:
#   PUBLISH_REPO     where to push, e.g. git@github.com:someone/dubplate.git (or a local path, for a test)
#   PUBLISH_NAME     the public author name
#   PUBLISH_EMAIL    the public author email (a GitHub noreply address keeps any real one out)
#   PUBLISH_DENY     words that must never appear, one per line (case-insensitive): real email, user names, host
#                    names... Kept as a CI secret, since writing them in the repository would publish them.
#   PUBLISH_SSH_KEY  a deploy key with write access (optional: without it, ssh uses whatever is set up)
#   PUBLISH_TAG      a tag to put on the published commit too, e.g. v0.8.0 (optional; starts GitHub's release job).
#                    In Woodpecker it is the tag that started the run (CI_COMMIT_TAG) when there is one.
#   PUBLISH_DRY_RUN  1 = do everything but the push
set -euo pipefail

die() { echo "publish: $*" >&2; exit 1; }
: "${PUBLISH_REPO:?set PUBLISH_REPO}" "${PUBLISH_NAME:?set PUBLISH_NAME}" "${PUBLISH_EMAIL:?set PUBLISH_EMAIL}"
[ -n "${PUBLISH_DENY:-}" ] || die "PUBLISH_DENY is empty: give it the private words to look for (one per line)"

PUBLISH_TAG=${PUBLISH_TAG:-${CI_COMMIT_TAG:-}}
src=$(git rev-parse --show-toplevel)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -n "${PUBLISH_SSH_KEY:-}" ]; then
    install -m 600 /dev/null "$work/key"
    printf '%s\n' "$PUBLISH_SSH_KEY" > "$work/key"
    # GitHub's published host key (https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints)
    echo 'github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl' > "$work/known_hosts"
    export GIT_SSH_COMMAND="ssh -i $work/key -o IdentitiesOnly=yes -o UserKnownHostsFile=$work/known_hosts -o StrictHostKeyChecking=yes"
fi

# ── the files ──────────────────────────────────────────────────────────────────────────────────────────────────────
mkdir "$work/tree"
git -C "$src" archive --format=tar HEAD | tar -x -C "$work/tree"
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$work/tree/Cargo.toml" | head -1)
message="Dubplate ${version:-snapshot} ($(date -u +%Y-%m-%d))"
if [ -n "${PUBLISH_TAG:-}" ]; then message="Dubplate ${PUBLISH_TAG}"; fi

# ── what must not go ───────────────────────────────────────────────────────────────────────────────────────────────
# Binary files are searched too (a .pyc, for one, holds the absolute path of its source).
# (Each pattern is written so that it does not match itself, or this script would stop its own publish.)
# Fixed patterns: anyone's home folder (made-up ones such as /home/dj or /Users/me are allowed), this machine's
# development folders, Claude's scratch folders, and private network addresses.
scan() {
    local where=$1 hits=0
    local fixed='(/home/(?!(dj|me|user|username)\b)[A-Za-z0-9._-]+|/Users/(?!(dj|me|user|username)\b)[A-Za-z0-9._-]+|[A-Za-z]:\\\\?Users\\\\?(?!(dj|me|user|username)\b)[A-Za-z0-9._-]+|/mn[t]/[A-Za-z0-9@._-]+/|/tmp/clau[d]e-|\.clau[d]e/projects|\b10\.\d{1,3}\.\d{1,3}\.\d{1,3}\b|\b172\.(1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}\b|\b192\.168\.\d{1,3}\.\d{1,3}\b)'
    if grep -raoP ".{0,60}$fixed.{0,60}" "$where" | sed "s|^$work/tree/||" | cut -c1-200; then hits=1; fi
    while IFS= read -r word; do
        word=$(printf '%s' "$word" | tr -d '\r' | sed 's/^ *//; s/ *$//')
        [ -n "$word" ] || continue
        # the word itself is never printed, only where it was found
        if grep -raliF -- "$word" "$where" | sed "s|^$work/tree/||; s|^|private word in: |"; then hits=1; fi
        if [ -d "$where" ] && find "$where" -iname "*${word}*" | grep -q .; then echo "private word in a file name"; hits=1; fi
    done <<< "$PUBLISH_DENY"
    return $hits
}
printf '%s\n%s\n%s\n' "$message" "$PUBLISH_NAME" "$PUBLISH_EMAIL" > "$work/message"
scan "$work/tree" || die "the files hold something private (above); nothing was published"
scan "$work/message" || die "the commit message or public identity holds something private; nothing was published"

# ── the commit ─────────────────────────────────────────────────────────────────────────────────────────────────────
export GIT_AUTHOR_NAME="$PUBLISH_NAME" GIT_AUTHOR_EMAIL="$PUBLISH_EMAIL"
export GIT_COMMITTER_NAME="$PUBLISH_NAME" GIT_COMMITTER_EMAIL="$PUBLISH_EMAIL"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1   # no private identity or settings from this machine
export TZ=UTC                                               # the commit's time zone would say where it was made

for attempt in 1 2 3; do
    rm -rf "$work/pub"
    git init -q -b main "$work/pub"
    git -C "$work/pub" remote add origin "$PUBLISH_REPO"
    if git -C "$work/pub" fetch -q --depth 1 origin main 2>/dev/null; then
        git -C "$work/pub" reset -q --soft FETCH_HEAD
        parent=$(git -C "$work/pub" rev-parse FETCH_HEAD)
    else
        echo "publish: $PUBLISH_REPO has no main yet; this is its first commit"
        parent=
    fi
    # the index becomes exactly the new tree: files gone from it are deleted on GitHub too
    git -C "$work/pub" --work-tree="$work/tree" add -A --force   # --force: the tree's own .gitignore must not drop files
    tree=$(git -C "$work/pub" write-tree)
    if [ -n "$parent" ] && [ "$tree" = "$(git -C "$work/pub" rev-parse "$parent^{tree}")" ]; then
        echo "publish: GitHub already has these files"
        commit=$parent
    else
        commit=$(git -C "$work/pub" commit-tree "$tree" ${parent:+-p "$parent"} -m "$message")
        git -C "$work/pub" --no-pager show --stat --format='%h %an <%ae>%n%s' "$commit" | tail -n 25
    fi

    refs=("$commit:refs/heads/main")
    if [ -n "${PUBLISH_TAG:-}" ]; then
        git -C "$work/pub" tag -a "$PUBLISH_TAG" -m "$message" "$commit"
        refs+=("refs/tags/$PUBLISH_TAG")
    fi
    if [ "${PUBLISH_DRY_RUN:-0}" = 1 ]; then
        echo "publish: dry run, would push ${refs[*]} to $PUBLISH_REPO"
        exit 0
    fi
    if [ "$commit" = "$parent" ] && [ -z "${PUBLISH_TAG:-}" ]; then exit 0; fi
    # not forced: if GitHub moved meanwhile (another publish), start again from its new main
    if git -C "$work/pub" push -q origin "${refs[@]}"; then
        echo "publish: pushed to $PUBLISH_REPO"
        exit 0
    fi
    echo "publish: push refused (attempt $attempt); fetching again"
    sleep 5
done
die "could not push after 3 attempts"
