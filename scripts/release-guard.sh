#!/usr/bin/env bash
# The release workflow's first job: refuse a tag that is not a finished,
# CI-tested release on main.
#
#   scripts/release-guard.sh vX.Y.Z
#
# Run from a full checkout of the tag (fetch-depth 0). Fails unless:
#
#   - the checkout is the commit the tag names,
#   - the tag is "v" plus Cargo.toml's version,
#   - CHANGELOG.md has a "## [X.Y.Z]" section,
#   - the commit is on origin/main, and
#   - every job in REQUIRED_JOBS passed in every ci.yml run on the commit.
#
# The tag and its commit are usually pushed together, so CI may not have
# started yet. The guard waits for the required jobs, and fails as soon as
# one of them fails, if a finished run lacks one, or if no CI run appears.
#
# On success, if GITHUB_OUTPUT is set, it writes two outputs for the later
# jobs: prerelease=true for a tag with a hyphen (v0.15.0-rc.1), and
# newest=true when the tag is the highest release tag that isn't a
# prerelease. Only the newest release becomes GitHub's latest and updates
# the Homebrew tap, so re-running an old tag can't downgrade either.
#
# Needs git, jq and a gh that can read the repository's runs. The waits are
# in seconds: GUARD_POLL between checks (30), GUARD_FIRST_RUN for a CI run to
# appear (300) and GUARD_TIMEOUT for the required jobs to finish (1800).
# tests/release_guard.rs shortens them and puts a fake gh on PATH.
set -euo pipefail

TAG="${1:?usage: release-guard.sh vX.Y.Z}"
REPO="${GITHUB_REPOSITORY:-matthart1983/syswatch}"
POLL="${GUARD_POLL:-30}"
FIRST_RUN="${GUARD_FIRST_RUN:-300}"
TIMEOUT="${GUARD_TIMEOUT:-1800}"

# The ci.yml jobs a release waits for, by the names GitHub gives them. The
# Windows test job runs in CI but is left out, because no Windows binary
# ships. A required job that a finished run doesn't have fails the guard,
# so renaming one in ci.yml can't quietly drop it from this list.
REQUIRED_JOBS=(
    "test (ubuntu-latest)"
    "test (macos-latest)"
    "release-guard"
    "cargo-deny"
    "msrv"
)

# `::error::` marks the line as the failure in the Actions log.
die() { echo "::error::$*"; exit 1; }

sha=$(git rev-parse HEAD)
tagged=$(git rev-parse -q --verify "refs/tags/${TAG}^{commit}") ||
    die "${TAG} is not a tag in this repository. Push the tag first; a release never moves or makes one."
[ "${tagged}" = "${sha}" ] ||
    die "the checkout is ${sha}, but ${TAG} names ${tagged}"

version=$(awk -F'"' '/^\[/ { pkg = ($0 == "[package]") } pkg && /^version *=/ { print $2; exit }' Cargo.toml)
[ "v${version}" = "${TAG}" ] || die "${TAG} is not the crate's version: Cargo.toml says ${version}"
awk -v h="## [${version}]" 'index($0, h) == 1 { found = 1 } END { exit !found }' CHANGELOG.md ||
    die "CHANGELOG.md has no '## [${version}]' heading"

git merge-base --is-ancestor "${sha}" origin/main ||
    die "${sha} is not on origin/main. Merge it first, then tag the merge."

required=$(printf '%s\n' "${REQUIRED_JOBS[@]}" | jq -R . | jq -s .)
start=$(date +%s)
while :; do
    runs=$(gh run list -R "${REPO}" --workflow ci.yml --commit "${sha}" \
        --json databaseId,status,conclusion,event,url)
    total=$(jq length <<< "${runs}")
    waiting=0
    failed=""
    for id in $(jq -r '.[].databaseId' <<< "${runs}"); do
        run=$(gh run view "${id}" -R "${REPO}" --json status,url,jobs)
        # One line per required job that hasn't passed: "wait" while it can
        # still pass, "missing" if the finished run doesn't have it, or
        # "failed" and how it ended.
        states=$(jq -r --argjson required "${required}" '
            .status as $run | .url as $url | .jobs as $jobs
            | $required[]
            | . as $name
            | [$jobs[] | select(.name == $name)][0] as $job
            | if $job == null then
                  if $run == "completed" then "missing \($name): not in the run \($url)" else "wait" end
              elif $job.status != "completed" then "wait"
              elif $job.conclusion == "success" then empty
              else "failed \($name): \($job.conclusion) in \($url)"
              end' <<< "${run}")
        while IFS= read -r state; do
            case "${state}" in
                "") ;;
                wait) waiting=$((waiting + 1)) ;;
                missing\ *)
                    failed+="  ${state#missing }"$'\n'
                    failed+="    (ci.yml has no job of that name, or the run never started)"$'\n' ;;
                *)
                    failed+="  ${state#failed }"$'\n'
                    failed+="    gh run rerun ${id} --failed"$'\n' ;;
            esac
        done <<< "${states}"
    done
    elapsed=$(( $(date +%s) - start ))
    if [ -n "${failed}" ]; then
        printf '%s' "${failed}"
        # A later run on the commit does not clear a failed one: every run
        # has to pass. Re-running the failed jobs replaces their result.
        die "CI did not pass on ${sha}. Fix it and tag the fix; or, if a job failed by chance, re-run it with the command above, then re-run this workflow."
    fi
    if [ "${total}" -gt 0 ] && [ "${waiting}" -eq 0 ]; then
        jq -r '.[] | "\(.event) \(.url)"' <<< "${runs}"
        break
    fi
    if [ "${total}" -eq 0 ] && [ "${elapsed}" -ge "${FIRST_RUN}" ]; then
        die "No CI run for ${sha}. ci.yml runs by itself only on the head of a push to main; for any other commit, run 'gh workflow run ci.yml --ref ${TAG}', then re-run this workflow once it passes."
    fi
    if [ "${elapsed}" -ge "${TIMEOUT}" ]; then
        die "CI on ${sha} was still running after ${TIMEOUT} seconds"
    fi
    echo "Waiting on CI for ${sha}: ${total} run(s), ${waiting} required job(s) not finished"
    sleep "${POLL}"
done

prerelease=false
[[ "${TAG}" == *-* ]] && prerelease=true
newest=$(git tag --list 'v*' | { grep -v -- - || true; } | sort -V | tail -n 1)
is_newest=false
[ "${TAG}" = "${newest}" ] && is_newest=true
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    echo "prerelease=${prerelease}" >> "${GITHUB_OUTPUT}"
    echo "newest=${is_newest}" >> "${GITHUB_OUTPUT}"
fi

echo "${TAG} is the crate's version, has a CHANGELOG section, is on main and passed CI (prerelease=${prerelease}, newest=${is_newest})"
