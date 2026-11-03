#!/usr/bin/env bash
# Make a release commit and its tag, locally.
#
#   scripts/release.sh 0.14.5 "what this release is, for the commit subject"
#
# Bumps the version in Cargo.toml and Cargo.lock. Renames CHANGELOG.md's
# `## [Unreleased]` to `## [X.Y.Z] - <today>` and puts a new, empty
# `## [Unreleased]` above it, so notes merged after the release don't land in
# the released version's section. Then it runs `cargo test`, commits
# "Release vX.Y.Z: <summary>" and makes an annotated tag. It pushes nothing;
# the push command is printed at the end. The release workflow
# (scripts/release-guard.sh) then refuses the tag unless it matches
# Cargo.toml, has a CHANGELOG section, is on main and passed CI.
#
# Refuses to start on a dirty tree, a detached HEAD, a branch other than
# main, a main behind origin/main, a tag that already exists, a version not
# above the current one, or an `[Unreleased]` section with no notes.
# Pre-releases (X.Y.Z-rc.N) are tagged by hand.
#
# package.nix is not touched: it reads its version from Cargo.toml.
set -euo pipefail

usage="usage: release.sh X.Y.Z \"summary\""
VERSION="${1:?${usage}}"
SUMMARY="${2:?${usage}}"
TAG="v${VERSION}"

die() { echo "release.sh: $*" >&2; exit 1; }

# rewrite FILE AWK-PROGRAM: replace FILE with the program's output. Not
# `sed -i`, which takes different arguments on macOS.
rewrite() {
    awk "$2" "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# newer A B: is X.Y.Z version A above B? `sort -V` is GNU-only.
newer() {
    local -a a b
    local i
    IFS=. read -ra a <<< "$1"
    IFS=. read -ra b <<< "$2"
    for i in 0 1 2; do
        [ "${a[i]}" -gt "${b[i]}" ] && return 0
        [ "${a[i]}" -lt "${b[i]}" ] && return 1
    done
    return 1
}

[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    die "${VERSION} is not X.Y.Z (no leading v; pre-releases are tagged by hand)"

root=$(git rev-parse --show-toplevel)
cd "${root}"

branch=$(git symbolic-ref --quiet --short HEAD) || die "HEAD is detached; check out main"
[ "${branch}" = main ] || die "on ${branch}; releases are made on main"

if [ -n "$(git status --porcelain)" ]; then
    git status --short >&2
    die "the working tree is not clean; commit or remove the changes above first"
fi

# Behind origin/main, the release commit could not be pushed without a merge,
# and the tag would name a commit that never reaches main.
if git remote get-url origin > /dev/null 2>&1; then
    git fetch --quiet origin "+refs/heads/main:refs/remotes/origin/main" ||
        die "could not fetch origin/main"
    behind=$(git rev-list --count "HEAD..refs/remotes/origin/main")
    [ "${behind}" -eq 0 ] || die "main is ${behind} commit(s) behind origin/main; pull first"
fi

git rev-parse --quiet --verify "refs/tags/${TAG}" > /dev/null && die "${TAG} already exists"

current=$(awk -F'"' '/^\[/ { pkg = ($0 == "[package]") } pkg && /^version *=/ { print $2; exit }' Cargo.toml)
[ -n "${current}" ] || die "no version in Cargo.toml's [package]"
# A pre-release (0.15.0-rc.1) may be followed by its own X.Y.Z.
core="${current%%-*}"
if ! newer "${VERSION}" "${core}" && ! { [ "${VERSION}" = "${core}" ] && [ "${current}" != "${core}" ]; }; then
    die "${VERSION} is not above the current version, ${current}"
fi

grep -q "^## \[${VERSION}\]" CHANGELOG.md && die "CHANGELOG.md already has a ${VERSION} section"
unreleased=$(awk '/^## \[Unreleased\][[:space:]]*$/ { on = 1; next }
    on && /^## \[/ { exit }
    on && NF && !/^#/ { n++ }
    END { print n + 0 }' CHANGELOG.md)
[ "${unreleased}" -gt 0 ] ||
    die "CHANGELOG.md has no '## [Unreleased]' section, or no notes in it; write the release notes there first"

echo "==> ${current} -> ${VERSION}"

# Handed to awk through the environment: `awk -v` would read backslashes as
# escapes.
TODAY=$(date +%F)
export V="${VERSION}" TODAY

# shellcheck disable=SC2016 # awk programs, not shell expansions
rewrite Cargo.toml '/^\[/ { pkg = ($0 == "[package]") }
    pkg && !done && /^version *=/ { $0 = "version = \"" ENVIRON["V"] "\""; done = 1 }
    { print }'
# shellcheck disable=SC2016
rewrite CHANGELOG.md '!done && /^## \[Unreleased\][[:space:]]*$/ {
        print "## [Unreleased]"; print ""
        $0 = "## [" ENVIRON["V"] "] - " ENVIRON["TODAY"]; done = 1 }
    { print }'

# --workspace rewrites only this crate's own entry in the lockfile.
cargo update --workspace --quiet
grep -A1 '^name = "syswatch"$' Cargo.lock | grep -qx "version = \"${VERSION}\"" ||
    die "Cargo.lock did not take ${VERSION}"

echo "==> cargo test"
cargo test ||
    die "cargo test failed; the version edits are left uncommitted (git checkout -- . discards them)"

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit --quiet -m "Release ${TAG}: ${SUMMARY}"
git tag -a "${TAG}" -m "syswatch ${TAG}"

echo
echo "Committed and tagged ${TAG} locally. Nothing has been pushed. To release:"
echo "  git push --atomic origin main ${TAG}"
