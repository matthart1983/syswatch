//! The release machinery: what `.github/workflows/release.yml` checks,
//! through `scripts/release-guard.sh`, before it builds anything, and
//! `scripts/release.sh`, which makes a commit and tag that pass those checks.
//!
//! v0.7.0 and v0.7.1 shipped from commits whose CI had failed, because
//! nothing tied a release to CI. The script tests need bash, git and jq, and
//! the workflow tests read `.github`, which package.nix leaves out of the Nix
//! build's source. Both are ignored by default, and CI's `release-guard` job
//! runs them with `--include-ignored`.

use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The guard's second check, made here too so a version bump without its
/// CHANGELOG section fails before anything is tagged.
#[test]
fn changelog_has_a_section_for_the_crate_version() {
    let heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
    assert!(
        include_str!("../CHANGELOG.md")
            .lines()
            .any(|l| l.starts_with(&heading)),
        "CHANGELOG.md has no `{heading}` section; the release workflow refuses a tag without one"
    );
}

/// The jobs in a workflow, each with its lines. Reads this repository's
/// layout (job names at two spaces, their keys at four); not a YAML parser.
fn jobs(workflow: &str) -> Vec<(&str, Vec<&str>)> {
    let mut jobs: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut in_jobs = false;
    for line in workflow.lines() {
        if !line.is_empty() && !line.starts_with([' ', '#']) {
            in_jobs = line == "jobs:";
            continue;
        }
        if !in_jobs {
            continue;
        }
        match line.strip_prefix("  ").and_then(|l| l.strip_suffix(':')) {
            Some(name) if !name.starts_with([' ', '#']) => jobs.push((name, Vec::new())),
            _ => {
                if let Some((_, body)) = jobs.last_mut() {
                    body.push(line);
                }
            }
        }
    }
    jobs
}

/// The jobs a job's `needs:` names, in either the scalar or the `[a, b]` form.
fn needs<'a>(body: &[&'a str]) -> Vec<&'a str> {
    body.iter()
        .filter_map(|l| l.strip_prefix("    needs:"))
        .flat_map(|n| {
            n.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
        })
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect()
}

/// The value of a job's four-space key, such as `if:`.
fn key<'a>(body: &[&'a str], name: &str) -> Option<&'a str> {
    body.iter()
        .find_map(|l| l.strip_prefix(&format!("    {name}:")))
        .map(str::trim)
}

#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn every_release_job_waits_for_the_guard() {
    let workflow = read(".github/workflows/release.yml");
    let jobs = jobs(&workflow);
    let needs_of = |job: &str| {
        jobs.iter()
            .find(|(name, _)| *name == job)
            .map(|(_, body)| needs(body))
            .unwrap_or_default()
    };
    assert!(
        jobs.iter().any(|(name, _)| *name == "guard"),
        "release.yml has no guard job"
    );
    assert!(jobs.len() > 1, "found no jobs besides the guard: {jobs:?}");
    for (job, _) in jobs.iter().filter(|(name, _)| *name != "guard") {
        let mut seen = vec![*job];
        let mut pending = needs_of(job);
        let mut guarded = false;
        while let Some(next) = pending.pop() {
            if next == "guard" {
                guarded = true;
                break;
            }
            if !seen.contains(&next) {
                seen.push(next);
                pending.extend(needs_of(next));
            }
        }
        assert!(
            guarded,
            "release.yml's `{job}` job does not need `guard`, directly or through another \
             job, so it would run on a tag whose commit CI never passed"
        );
    }
}

/// Without `actions: read` the guard's `gh run list` fails instead of
/// reporting CI's runs, and the top-level permissions leave it at none.
#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn the_guard_can_read_ci_runs() {
    let workflow = read(".github/workflows/release.yml");
    let jobs = jobs(&workflow);
    let (_, guard) = jobs
        .iter()
        .find(|(name, _)| *name == "guard")
        .expect("release.yml has a guard job");
    assert!(
        guard.iter().any(|l| l.trim() == "actions: read"),
        "the guard job needs `actions: read` to list CI's runs"
    );
    assert!(
        guard
            .iter()
            .any(|l| l.trim() == r#"run: scripts/release-guard.sh "${RELEASE_TAG}""#),
        "the guard job does not run scripts/release-guard.sh, whose checks the tests below cover"
    );
}

/// The ci.yml jobs scripts/release-guard.sh waits for.
fn required_jobs() -> Vec<String> {
    let script = read("scripts/release-guard.sh");
    let list = script
        .split_once("REQUIRED_JOBS=(\n")
        .and_then(|(_, rest)| rest.split_once("\n)"))
        .expect("release-guard.sh has a REQUIRED_JOBS=( ... ) list")
        .0;
    list.lines()
        .map(|l| l.trim().trim_matches('"').to_string())
        .collect()
}

/// Every job the guard requires is a job ci.yml runs, under the name GitHub
/// gives it. Windows runs in ci.yml but doesn't hold a release, and Nix has
/// its own workflow, which the guard doesn't read.
#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn a_release_waits_for_every_platform_it_ships_and_not_for_windows_or_nix() {
    let ci = read(".github/workflows/ci.yml");
    let ci_jobs = jobs(&ci);
    let mut names = Vec::new();
    for (job, body) in &ci_jobs {
        let os: Vec<&str> = body
            .iter()
            .find_map(|l| l.trim().strip_prefix("os: ["))
            .map(|l| l.trim_end_matches(']').split(',').map(str::trim).collect())
            .unwrap_or_default();
        if os.is_empty() {
            names.push(job.to_string());
        } else {
            assert_eq!(
                key(body, "name"),
                Some("test (${{ matrix.os }})"),
                "ci.yml's `{job}` job names its matrix jobs some other way than REQUIRED_JOBS expects"
            );
            names.extend(os.iter().map(|os| format!("{job} ({os})")));
        }
    }

    let required = required_jobs();
    for job in &required {
        assert!(
            names.contains(job),
            "release-guard.sh requires `{job}`, which ci.yml doesn't run; every tag would fail"
        );
    }
    for job in [
        "test (ubuntu-latest)",
        "test (macos-latest)",
        "release-guard",
        "cargo-deny",
        "msrv",
    ] {
        assert!(
            required.iter().any(|r| r == job),
            "a release no longer waits for `{job}`"
        );
    }
    assert!(names.iter().any(|n| n == "test (windows-latest)"));
    assert!(
        !required.iter().any(|r| r.contains("windows")),
        "Windows gates releases, but no Windows binary ships"
    );
    assert!(
        !ci_jobs.iter().any(|(name, _)| name.contains("nix")),
        "Nix belongs in nix.yml, outside the guard's view"
    );
    assert!(
        jobs(&read(".github/workflows/nix.yml")).len() == 1,
        "nix.yml should have its one job"
    );
}

/// A tag with a hyphen is a pre-release: a GitHub pre-release, never on
/// crates.io, never `latest`. Only the newest release becomes `latest` or
/// moves the Homebrew tap, so re-running an older tag downgrades neither.
/// release-guard.sh computes both outputs; the tests below cover it.
#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn prereleases_and_old_tags_publish_nothing_they_shouldnt() {
    let workflow = read(".github/workflows/release.yml");
    let jobs = jobs(&workflow);
    let body = |job: &str| {
        jobs.iter()
            .find(|(name, _)| *name == job)
            .map(|(_, body)| body.clone())
            .unwrap_or_else(|| panic!("release.yml has no `{job}` job"))
    };
    let guard = body("guard");
    for output in ["prerelease", "newest"] {
        assert!(
            guard
                .iter()
                .any(|l| l.trim() == format!("{output}: ${{{{ steps.guard.outputs.{output} }}}}")),
            "the guard job does not pass on its `{output}` output"
        );
    }
    assert_eq!(
        key(&body("publish"), "if"),
        Some("needs.guard.outputs.prerelease == 'false'")
    );
    assert_eq!(
        key(&body("homebrew"), "if"),
        Some("needs.guard.outputs.newest == 'true'")
    );
    let release = body("release");
    let with = |k: &str| {
        release
            .iter()
            .find_map(|l| l.trim().strip_prefix(&format!("{k}:")))
            .map(str::trim)
    };
    assert_eq!(
        with("prerelease"),
        Some("${{ needs.guard.outputs.prerelease }}")
    );
    assert_eq!(
        with("make_latest"),
        Some("${{ needs.guard.outputs.newest }}")
    );
    // A job-level continue-on-error turns a real publish failure, such as a
    // 403 from an expired token, into a green run.
    assert!(
        !workflow.contains("continue-on-error"),
        "release.yml should fail when a step fails"
    );
}

/// `cargo publish` refuses a checkout with uncommitted files in it, so the
/// publish step's log has to live outside the checkout. v0.14.5's first
/// publish failed on a `publish.log` written beside Cargo.toml.
#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn the_publish_log_is_written_outside_the_checkout() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains("log=\"$RUNNER_TEMP/publish.log\""),
        "the publish step should log to $RUNNER_TEMP"
    );
    for line in workflow.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        assert!(
            !line.contains("> publish.log") && !line.contains(" publish.log;"),
            "release.yml writes or reads publish.log in the checkout: {line}"
        );
    }
}

/// On a dispatch, github.ref is a branch. Every checkout of this repository
/// has to name the tag, or a re-run would build main.
#[test]
#[ignore = "reads .github, which the Nix build's source leaves out"]
fn every_release_checkout_is_of_the_tag() {
    let workflow = read(".github/workflows/release.yml");
    let lines: Vec<&str> = workflow.lines().collect();
    let mut checkouts = 0;
    for (i, line) in lines.iter().enumerate() {
        if !line.trim().starts_with("- uses: actions/checkout@")
            && !line.trim().starts_with("uses: actions/checkout@")
        {
            continue;
        }
        checkouts += 1;
        let with: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| !l.trim().starts_with("- ") && !l.trim().is_empty())
            .map(|l| l.trim())
            .collect();
        assert!(
            with.contains(&"ref: ${{ env.RELEASE_TAG }}")
                || with.contains(&"repository: matthart1983/homebrew-tap"),
            "release.yml line {}: a checkout without `ref: ${{{{ env.RELEASE_TAG }}}}`",
            i + 1
        );
    }
    assert!(checkouts >= 4, "found {checkouts} checkouts");
}

/// scripts/release-guard.sh and scripts/release.sh, run in throwaway
/// repositories. Unix only, like the scripts.
#[cfg(unix)]
mod scripts {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The jobs scripts/release-guard.sh requires, all passed, plus Windows.
    const REQUIRED: [&str; 5] = [
        "test (ubuntu-latest)",
        "test (macos-latest)",
        "release-guard",
        "cargo-deny",
        "msrv",
    ];
    const WINDOWS: &str = "test (windows-latest)";

    /// A CI run as `gh run view` reports it.
    struct Run {
        id: u64,
        event: &'static str,
        status: &'static str,
        jobs: Vec<(&'static str, &'static str, &'static str)>,
    }

    impl Run {
        /// A finished run in which every job passed.
        fn green(id: u64) -> Self {
            Self::finished(id, &[])
        }

        /// A finished run in which `outcomes` name the jobs that didn't
        /// pass, and how they ended. Every other job passed.
        fn finished(id: u64, outcomes: &[(&'static str, &'static str)]) -> Self {
            let jobs = REQUIRED
                .iter()
                .chain([&WINDOWS])
                .map(|&name| {
                    let conclusion = outcomes
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map_or("success", |(_, c)| *c);
                    (name, "completed", conclusion)
                })
                .collect();
            Run {
                id,
                event: "push",
                status: "completed",
                jobs,
            }
        }

        fn list_entry(&self) -> String {
            format!(
                r#"{{"databaseId":{},"status":"{}","conclusion":"","event":"{}","url":"https://github.com/owner/syswatch/actions/runs/{}"}}"#,
                self.id, self.status, self.event, self.id
            )
        }

        fn view(&self) -> String {
            let jobs: Vec<String> = self
                .jobs
                .iter()
                .map(|(name, status, conclusion)| {
                    format!(
                        r#"{{"name":"{name}","status":"{status}","conclusion":"{conclusion}"}}"#
                    )
                })
                .collect();
            format!(
                r#"{{"status":"{}","url":"https://github.com/owner/syswatch/actions/runs/{}","jobs":[{}]}}"#,
                self.status,
                self.id,
                jobs.join(",")
            )
        }
    }

    /// What a run of release-guard.sh did.
    struct Guarded {
        passed: bool,
        log: String,
        /// The arguments of each call to `gh`.
        gh: Vec<String>,
        /// What it wrote to GITHUB_OUTPUT.
        outputs: String,
    }

    impl Guarded {
        fn list_calls(&self) -> usize {
            self.gh.iter().filter(|c| c.starts_with("run list")).count()
        }
    }

    fn has(tool: &str) -> bool {
        Command::new(tool).arg("--version").output().is_ok()
    }

    /// A throwaway repository shaped like this one, as far as the release
    /// scripts can tell: a crate named syswatch at 0.14.4 with no
    /// dependencies, tagged v0.14.4 on main, with origin/main at the tag.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            for tool in ["bash", "git", "jq", "awk"] {
                assert!(has(tool), "the release tests need {tool}");
            }
            static N: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "syswatch-release-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            let repo = root.join("repo");
            fs::create_dir_all(repo.join("src")).unwrap();
            fs::create_dir_all(repo.join("scripts")).unwrap();
            fs::write(
                repo.join("Cargo.toml"),
                "[package]\nname = \"syswatch\"\nversion = \"0.14.4\"\nedition = \"2021\"\n\n\
                 [dependencies]\n",
            )
            .unwrap();
            fs::write(repo.join("src/lib.rs"), "").unwrap();
            fs::write(repo.join(".gitignore"), "/target\n").unwrap();
            fs::write(
                repo.join("CHANGELOG.md"),
                "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- Something.\n\n\
                 ## [0.14.4] - 2026-10-17\n\n### Fixed\n\n- Something earlier.\n",
            )
            .unwrap();
            for script in ["scripts/release.sh", "scripts/release-guard.sh"] {
                fs::copy(super::root().join(script), repo.join(script)).unwrap();
            }
            let fixture = Self(root);
            let lock = fixture
                .command("cargo")
                .arg("generate-lockfile")
                .output()
                .unwrap();
            assert!(lock.status.success(), "{lock:?}");
            fixture.git(&["init", "-q", "-b", "main"]);
            fixture.git(&["config", "user.name", "Release Test"]);
            fixture.git(&["config", "user.email", "release-test@example.com"]);
            fixture.git(&["add", "-A"]);
            fixture.git(&["commit", "-q", "-m", "Release v0.14.4: earlier"]);
            fixture.git(&["tag", "v0.14.4"]);
            fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
            fixture
        }

        fn repo(&self) -> PathBuf {
            self.0.join("repo")
        }

        /// Isolated from the user's git configuration, which may sign
        /// commits and tags or install hooks, and from GIT_DIR and the rest,
        /// which a git hook running `cargo test` would pass down. Isolated
        /// too from the Actions variables of the CI job running these tests.
        fn command(&self, program: &str) -> Command {
            let mut cmd = Command::new(program);
            for (key, _) in std::env::vars_os() {
                let key_str = key.to_string_lossy();
                if key_str.starts_with("GIT_") || key_str.starts_with("GITHUB_") {
                    cmd.env_remove(key);
                }
            }
            cmd.current_dir(self.repo())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("CARGO_TARGET_DIR", self.0.join("target"));
            cmd
        }

        fn git(&self, args: &[&str]) -> String {
            let out = self.command("git").args(args).output().unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }

        fn head(&self) -> String {
            self.git(&["rev-parse", "HEAD"]).trim().to_string()
        }

        fn read(&self, path: &str) -> String {
            fs::read_to_string(self.repo().join(path)).unwrap()
        }

        fn write(&self, path: &str, contents: &str) {
            fs::write(self.repo().join(path), contents).unwrap();
        }

        /// Commits a release of `version` by hand, with its CHANGELOG
        /// section, tags it and puts origin/main there.
        fn released_by_hand(&self, version: &str) {
            let toml = self
                .read("Cargo.toml")
                .replace("version = \"0.14.4\"", &format!("version = \"{version}\""));
            self.write("Cargo.toml", &toml);
            let log = self.read("CHANGELOG.md").replace(
                "## [0.14.4]",
                &format!("## [{version}] - 2026-11-03\n\n- Notes.\n\n## [0.14.4]"),
            );
            self.write("CHANGELOG.md", &log);
            self.git(&["commit", "-q", "-am", &format!("Release v{version}")]);
            self.git(&["tag", &format!("v{version}")]);
            self.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        }

        /// Runs release-guard.sh for `tag` against a fake `gh`. Each poll
        /// of the guard (one `gh run list`, then `gh run view` per run) sees
        /// the next of `polls`, and the last one repeats. The guard doesn't
        /// sleep between polls, and gives CI 30 seconds to start and to
        /// finish unless `env` says otherwise.
        fn guard(&self, tag: &str, polls: &[Vec<Run>], env: &[(&str, &str)]) -> Guarded {
            let bin = self.0.join("bin");
            let _ = fs::remove_dir_all(&bin);
            fs::create_dir_all(&bin).unwrap();
            fs::write(
                bin.join("gh"),
                r#"#!/bin/sh
dir=$(dirname "$0")
echo "$*" >> "$dir/calls"
case "$1 $2" in
    "run list")
        n=$(( $(cat "$dir/poll" 2>/dev/null || echo 0) + 1 ))
        echo "$n" > "$dir/poll"
        [ -f "$dir/list.$n.json" ] || n=last
        cat "$dir/list.$n.json" ;;
    "run view")
        n=$(cat "$dir/poll")
        [ -f "$dir/list.$n.json" ] || n=last
        cat "$dir/view.$n.$3.json" ;;
    *)
        echo "fake gh: unexpected arguments: $*" >&2
        exit 2 ;;
esac
"#,
            )
            .unwrap();
            fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
            let names: Vec<String> = (1..=polls.len())
                .map(|n| n.to_string())
                .chain(["last".to_string()])
                .collect();
            for (n, poll) in names.iter().zip(polls.iter().chain(polls.last())) {
                let list: Vec<String> = poll.iter().map(Run::list_entry).collect();
                fs::write(
                    bin.join(format!("list.{n}.json")),
                    format!("[{}]", list.join(",")),
                )
                .unwrap();
                for run in poll {
                    fs::write(bin.join(format!("view.{n}.{}.json", run.id)), run.view()).unwrap();
                }
            }

            let outputs = self.0.join("github-output");
            let _ = fs::remove_file(&outputs);
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![bin.clone()];
            paths.extend(std::env::split_paths(&path));
            let out = self
                .command("bash")
                .arg("scripts/release-guard.sh")
                .arg(tag)
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env("GITHUB_REPOSITORY", "owner/syswatch")
                .env("GITHUB_OUTPUT", &outputs)
                .env("GUARD_POLL", "0")
                .env("GUARD_FIRST_RUN", "30")
                .env("GUARD_TIMEOUT", "30")
                .envs(env.iter().copied())
                .output()
                .unwrap();
            Guarded {
                passed: out.status.success(),
                log: format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
                gh: fs::read_to_string(bin.join("calls"))
                    .unwrap_or_default()
                    .lines()
                    .map(String::from)
                    .collect(),
                outputs: fs::read_to_string(&outputs).unwrap_or_default(),
            }
        }

        fn release(&self, args: &[&str]) -> Output {
            self.command("bash")
                .arg("scripts/release.sh")
                .args(args)
                .output()
                .unwrap()
        }

        /// Runs release.sh, expects a refusal naming `reason`, and checks
        /// that it left no commit, no new tag and no edit behind.
        fn refuses(&self, args: &[&str], reason: &str) {
            let head = self.head();
            let tags = self.git(&["tag", "--list"]);
            let status = self.git(&["status", "--porcelain"]);
            let out = self.release(args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                !out.status.success(),
                "release.sh {args:?} should have refused"
            );
            assert!(
                stderr.contains(reason),
                "release.sh {args:?}: expected {reason:?} in {stderr}"
            );
            assert_eq!(self.head(), head);
            assert_eq!(self.git(&["tag", "--list"]), tags);
            assert_eq!(self.git(&["status", "--porcelain"]), status);
        }

        /// Gives the repository a real origin one commit ahead of main.
        fn behind_origin(&self) {
            self.git(&["clone", "-q", "--bare", ".", "../origin.git"]);
            self.git(&["remote", "add", "origin", "../origin.git"]);
            self.git(&["commit", "-q", "--allow-empty", "-m", "on the remote"]);
            self.git(&["push", "-q", "-u", "origin", "main"]);
            self.git(&["reset", "-q", "--hard", "HEAD~1"]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ---- release-guard.sh ----

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_passes_a_tag_whose_required_jobs_passed() {
        let f = Fixture::new();
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(run.passed, "{}", run.log);
        assert_eq!(
            run.gh,
            [
                format!(
                    "run list -R owner/syswatch --workflow ci.yml --commit {} \
                     --json databaseId,status,conclusion,event,url",
                    f.head()
                ),
                "run view 101 -R owner/syswatch --json status,url,jobs".to_string(),
            ]
        );
    }

    /// Windows runs in CI, and its result shows on the pull request, but no
    /// Windows binary ships, so it doesn't hold a release, failed or slow.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_does_not_wait_for_windows() {
        let f = Fixture::new();
        let run = f.guard(
            "v0.14.4",
            &[vec![Run::finished(101, &[(WINDOWS, "failure")])]],
            &[],
        );
        assert!(run.passed, "{}", run.log);

        let mut slow = Run::green(102);
        slow.status = "in_progress";
        slow.jobs.iter_mut().find(|j| j.0 == WINDOWS).unwrap().1 = "in_progress";
        let run = f.guard("v0.14.4", &[vec![slow]], &[("GUARD_TIMEOUT", "0")]);
        assert!(run.passed, "{}", run.log);
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_tag_that_disagrees_with_cargo_toml() {
        let f = Fixture::new();
        f.git(&["tag", "v0.14.5"]);
        let run = f.guard("v0.14.5", &[vec![Run::green(101)]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log
                .contains("::error::v0.14.5 is not the crate's version: Cargo.toml says 0.14.4"),
            "{}",
            run.log
        );
        assert!(
            run.gh.is_empty(),
            "asked CI about a tag already refused: {:?}",
            run.gh
        );
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_release_with_no_changelog_section() {
        let f = Fixture::new();
        let log = f
            .read("CHANGELOG.md")
            .replace("## [0.14.4] - 2026-10-17", "## 0.14.4");
        f.write("CHANGELOG.md", &log);
        f.git(&["commit", "-q", "-am", "lose the heading"]);
        f.git(&["tag", "-f", "v0.14.4"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log
                .contains("::error::CHANGELOG.md has no '## [0.14.4]' heading"),
            "{}",
            run.log
        );
        assert!(run.gh.is_empty(), "{:?}", run.gh);
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_commit_that_is_not_on_main() {
        let f = Fixture::new();
        f.git(&["commit", "-q", "--allow-empty", "-m", "on a branch"]);
        f.git(&["tag", "-f", "v0.14.4"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log
                .contains(&format!("::error::{} is not on origin/main", f.head())),
            "{}",
            run.log
        );
        assert!(run.gh.is_empty(), "{:?}", run.gh);
    }

    /// A dispatch names a tag that has to exist already, and every job
    /// checks it out; the guard makes sure the checkout is that commit.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_tag_that_is_missing_or_not_checked_out() {
        let f = Fixture::new();
        f.git(&["tag", "-d", "v0.14.4"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log
                .contains("::error::v0.14.4 is not a tag in this repository"),
            "{}",
            run.log
        );

        f.git(&["tag", "v0.14.4"]);
        let tagged = f.head();
        f.git(&["commit", "-q", "--allow-empty", "-m", "later"]);
        f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log.contains(&format!(
                "::error::the checkout is {}, but v0.14.4 names {tagged}",
                f.head()
            )),
            "{}",
            run.log
        );
        assert!(run.gh.is_empty(), "{:?}", run.gh);
    }

    /// Each required job has to pass. A job that was skipped, cancelled or
    /// timed out did not pass.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_tag_whose_required_jobs_did_not_pass() {
        let f = Fixture::new();
        for job in REQUIRED {
            for conclusion in ["failure", "cancelled", "timed_out", "skipped"] {
                let run = f.guard(
                    "v0.14.4",
                    &[vec![Run::finished(101, &[(job, conclusion)])]],
                    &[],
                );
                assert!(!run.passed, "{job} {conclusion}: {}", run.log);
                assert!(
                    run.log.contains(&format!("{job}: {conclusion} in ")),
                    "{job} {conclusion}: {}",
                    run.log
                );
                assert!(run.log.contains("::error::CI did not pass"), "{}", run.log);
                assert!(run.log.contains("gh run rerun 101 --failed"), "{}", run.log);
            }
        }
    }

    /// A finished run that doesn't have a required job, because ci.yml
    /// renamed or dropped it, fails rather than passing for want of it.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_finished_run_without_a_required_job() {
        let f = Fixture::new();
        let mut run = Run::green(101);
        run.jobs.retain(|j| j.0 != "msrv");
        let run = f.guard("v0.14.4", &[vec![run]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log.contains("msrv: not in the run https://"),
            "{}",
            run.log
        );
        // Re-running the run can't add a job to it.
        assert!(!run.log.contains("gh run rerun"), "{}", run.log);

        // A run that failed to start has no jobs at all.
        let broken = Run {
            id: 102,
            event: "push",
            status: "completed",
            jobs: vec![],
        };
        let run = f.guard("v0.14.4", &[vec![broken]], &[]);
        assert!(!run.passed, "{}", run.log);
    }

    /// Every run on the commit has to pass: a later run that passed does not
    /// clear one that failed, and the guard stops at the first failure
    /// without waiting for the rest.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_refuses_a_failed_run_even_beside_a_green_one() {
        let f = Fixture::new();
        let failed = Run::finished(101, &[("cargo-deny", "failure")]);
        let mut passed = Run::green(102);
        passed.event = "workflow_dispatch";
        let run = f.guard("v0.14.4", &[vec![failed, passed]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert!(run.log.contains("gh run rerun 101 --failed"), "{}", run.log);
        assert!(!run.log.contains("gh run rerun 102"), "{}", run.log);

        let failed = Run::finished(101, &[("cargo-deny", "failure")]);
        let mut running = Run::green(102);
        running.status = "in_progress";
        running.jobs[0].1 = "in_progress";
        let run = f.guard("v0.14.4", &[vec![failed, running]], &[]);
        assert!(!run.passed, "{}", run.log);
        assert_eq!(run.list_calls(), 1, "{}", run.log);
    }

    /// The tag and its commit are pushed together, so CI may not have
    /// started when the guard first looks, and then may not have finished.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_waits_for_ci_to_start_and_finish() {
        let f = Fixture::new();
        let queued = Run {
            id: 101,
            event: "push",
            status: "queued",
            jobs: vec![],
        };
        let mut running = Run::green(101);
        running.status = "in_progress";
        running.jobs[1].1 = "in_progress";
        running.jobs[1].2 = "";
        let run = f.guard(
            "v0.14.4",
            &[vec![], vec![queued], vec![running], vec![Run::green(101)]],
            &[],
        );
        assert!(run.passed, "{}", run.log);
        assert_eq!(run.list_calls(), 4, "{}", run.log);
        assert!(run.log.contains("Waiting on CI"), "{}", run.log);
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_gives_up_on_ci_that_never_starts_or_never_finishes() {
        let f = Fixture::new();
        let run = f.guard("v0.14.4", &[vec![]], &[("GUARD_FIRST_RUN", "0")]);
        assert!(!run.passed, "{}", run.log);
        assert!(run.log.contains("::error::No CI run"), "{}", run.log);
        assert!(
            run.log.contains("gh workflow run ci.yml --ref v0.14.4"),
            "{}",
            run.log
        );

        let mut running = Run::green(101);
        running.status = "in_progress";
        running.jobs[0].1 = "in_progress";
        let run = f.guard("v0.14.4", &[vec![running]], &[("GUARD_TIMEOUT", "0")]);
        assert!(!run.passed, "{}", run.log);
        assert!(
            run.log.contains("still running after 0 seconds"),
            "{}",
            run.log
        );
    }

    /// Only the highest release tag becomes `latest` and moves the tap.
    /// Versions compare as numbers, so v0.14.4 is above v0.9.0.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_says_whether_the_tag_is_the_newest_release() {
        let f = Fixture::new();
        f.git(&["tag", "v0.9.0", "HEAD"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(run.passed, "{}", run.log);
        assert_eq!(run.outputs, "prerelease=false\nnewest=true\n");

        // A re-run of v0.14.4 once v0.14.10 is out.
        f.git(&["tag", "v0.14.10", "HEAD"]);
        let run = f.guard("v0.14.4", &[vec![Run::green(101)]], &[]);
        assert!(run.passed, "{}", run.log);
        assert_eq!(run.outputs, "prerelease=false\nnewest=false\n");
    }

    /// A tag with a hyphen is a pre-release, and never the newest release,
    /// even above every other tag.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn guard_marks_a_hyphenated_tag_as_a_prerelease() {
        let f = Fixture::new();
        f.released_by_hand("0.15.0-rc.1");
        let run = f.guard("v0.15.0-rc.1", &[vec![Run::green(101)]], &[]);
        assert!(run.passed, "{}", run.log);
        assert_eq!(run.outputs, "prerelease=true\nnewest=false\n");
    }

    // ---- release.sh ----

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_bumps_the_version_commits_and_tags() {
        let f = Fixture::new();
        let out = f.release(&["0.14.5", "gate releases on CI"]);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(f.read("Cargo.toml").contains("\nversion = \"0.14.5\"\n"));
        assert!(f
            .read("Cargo.lock")
            .contains("name = \"syswatch\"\nversion = \"0.14.5\"\n"));

        // The notes move under 0.14.5, and an empty [Unreleased] goes above
        // it for whatever merges next.
        let log = f.read("CHANGELOG.md");
        let date = log
            .lines()
            .find_map(|l| l.strip_prefix("## [0.14.5] - "))
            .expect("the Unreleased section became 0.14.5's");
        assert_eq!(date.len(), "YYYY-MM-DD".len(), "{date}");
        assert!(
            log.starts_with(&format!(
                "# Changelog\n\n## [Unreleased]\n\n## [0.14.5] - {date}\n\n### Fixed\n\n- Something.\n\n\
                 ## [0.14.4] - 2026-10-17\n"
            )),
            "{log}"
        );

        assert_eq!(
            f.git(&["log", "-1", "--format=%s"]).trim(),
            "Release v0.14.5: gate releases on CI"
        );
        assert_eq!(f.git(&["rev-parse", "v0.14.5^{commit}"]).trim(), f.head());
        assert_eq!(f.git(&["cat-file", "-t", "v0.14.5"]).trim(), "tag");
        assert_eq!(f.git(&["status", "--porcelain"]), "");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("git push --atomic origin main v0.14.5"),
            "{stdout}"
        );
    }

    /// What release.sh makes, the guard accepts once it is on main and CI
    /// has passed. The fresh [Unreleased] stops the next run of release.sh
    /// until there are notes in it.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn a_release_from_the_script_passes_the_guard() {
        let f = Fixture::new();
        let out = f.release(&["0.14.5", "summary"]);
        assert!(out.status.success(), "{out:?}");
        f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let run = f.guard("v0.14.5", &[vec![Run::green(101)]], &[]);
        assert!(run.passed, "{}", run.log);
        assert_eq!(run.outputs, "prerelease=false\nnewest=true\n");

        f.refuses(&["0.14.6", "summary"], "[Unreleased]");
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_refuses_a_dirty_tree() {
        let f = Fixture::new();
        f.write("src/lib.rs", "// unfinished\n");
        f.refuses(&["0.14.5", "summary"], "not clean");
        f.write("src/lib.rs", "");
        f.write("stray.txt", "");
        f.refuses(&["0.14.5", "summary"], "not clean");
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_refuses_anything_but_main() {
        let f = Fixture::new();
        f.git(&["checkout", "-q", "-b", "release/0.14.5"]);
        f.refuses(&["0.14.5", "summary"], "on release/0.14.5");
        f.git(&["checkout", "-q", "--detach"]);
        f.refuses(&["0.14.5", "summary"], "detached");
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_refuses_a_version_that_does_not_follow() {
        let f = Fixture::new();
        f.refuses(&["v0.14.5", "summary"], "is not X.Y.Z");
        f.refuses(&["0.15", "summary"], "is not X.Y.Z");
        f.refuses(&["0.15.0-rc.1", "summary"], "is not X.Y.Z");
        // Without its tag, which would be refused first.
        f.git(&["tag", "-d", "v0.14.4"]);
        f.refuses(&["0.14.4", "summary"], "is not above the current version");
        f.refuses(&["0.13.9", "summary"], "is not above the current version");
        f.refuses(&["0.14.5"], "usage");
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_refuses_an_existing_tag_or_missing_notes() {
        let f = Fixture::new();
        f.git(&["tag", "v0.14.5"]);
        f.refuses(&["0.14.5", "summary"], "v0.14.5 already exists");
        f.git(&["tag", "-d", "v0.14.5"]);

        let released = f.read("CHANGELOG.md").replace(
            "## [0.14.4]",
            "## [0.14.5] - 2026-11-03\n\n- Already out.\n\n## [0.14.4]",
        );
        f.write("CHANGELOG.md", &released);
        f.git(&["commit", "-q", "-am", "0.14.5 notes"]);
        f.refuses(&["0.14.5", "summary"], "already has a 0.14.5 section");
        f.git(&["reset", "-q", "--hard", "HEAD~1"]);

        // A heading is not a note.
        let heading_only = f
            .read("CHANGELOG.md")
            .replace("- Something.\n\n## [0.14.4]", "\n## [0.14.4]");
        f.write("CHANGELOG.md", &heading_only);
        f.git(&["commit", "-q", "-am", "a heading and no notes"]);
        f.refuses(&["0.14.5", "summary"], "[Unreleased]");

        let none = f.read("CHANGELOG.md").replace("## [Unreleased]\n\n", "");
        f.write("CHANGELOG.md", &none);
        f.git(&["commit", "-q", "-am", "no Unreleased section"]);
        f.refuses(&["0.14.5", "summary"], "[Unreleased]");
    }

    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_refuses_a_main_behind_origin() {
        let f = Fixture::new();
        f.behind_origin();
        f.refuses(
            &["0.14.5", "summary"],
            "main is 1 commit(s) behind origin/main",
        );
    }

    /// The release is tested before it is committed or tagged.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_makes_no_commit_or_tag_when_the_tests_fail() {
        let f = Fixture::new();
        f.write(
            "src/lib.rs",
            "#[test]\nfn red() {\n    panic!(\"red\");\n}\n",
        );
        f.git(&["commit", "-q", "-am", "a failing test"]);
        let head = f.head();
        let out = f.release(&["0.14.5", "summary"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "release.sh released a red commit");
        assert!(stderr.contains("cargo test failed"), "{stderr}");
        assert_eq!(f.head(), head);
        assert!(!f.git(&["tag", "--list"]).contains("v0.14.5"));
        // Left for a look, as the refusal says.
        assert!(f.git(&["status", "--porcelain"]).contains(" M Cargo.toml"));
    }

    /// A pre-release may be followed by its own X.Y.Z, and by nothing lower.
    #[test]
    #[ignore = "needs bash, git and jq"]
    fn release_script_follows_a_pre_release_with_its_release() {
        let f = Fixture::new();
        f.released_by_hand("0.15.0-rc.1");
        f.refuses(
            &["0.14.5", "summary"],
            "0.14.5 is not above the current version, 0.15.0-rc.1",
        );
        let out = f.release(&["0.15.0", "summary"]);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(f.read("Cargo.toml").contains("\nversion = \"0.15.0\"\n"));
        assert_eq!(f.git(&["rev-parse", "v0.15.0^{commit}"]).trim(), f.head());
    }
}
