# Changelog

Notable changes to syswatch, in the form of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html), so before 1.0 a
minor release can break things.

Add a note under `[Unreleased]` with each change. `scripts/release.sh` turns
that section into the release's, and the release workflow refuses a tag
without one and publishes it as the GitHub release notes.

The sections for 0.1.0 to 0.14.4 were written in November 2026 from the
release commits and GitHub's release notes, so they are short. Numbers in
brackets are issues and pull requests.

## [Unreleased]

### Changed

- The minimum supported Rust is now 1.89. `Cargo.toml` declares it, and CI
  builds on it.
- Updated dependencies, which clears the published advisories against
  anyhow, tokio, zstd, zstd-safe and zstd-sys. Removed tokio, which nothing
  used.
- The published crate leaves out the demo GIFs. It is 259 KB, down from
  6.1 MB.

### Fixed

- The Homebrew tap formula's test expected `syswatch --help` to exit 1. It
  exits 0.
- Dense no longer crashes in a terminal 3 to 8 rows tall, or on a key press
  in one, which used to leave the terminal in raw mode (#39).

### Release process

- A release now stops before it builds anything unless its tag matches
  `Cargo.toml`, this file has a section for it, the commit is on `main`, and
  CI's Linux, macOS, release-guard, cargo-deny and MSRV jobs passed on that
  commit. Windows CI still runs, but doesn't hold a release.
- `scripts/release.sh X.Y.Z "summary"` makes the release commit and its tag.
- A failed release can run again against the same tag:
  `gh workflow run release.yml --ref main -f tag=vX.Y.Z`. A version that is
  already on crates.io counts as published.
- A tag with a hyphen, such as `v0.15.0-rc.1`, makes a GitHub pre-release.
  It never goes to crates.io and never becomes the latest release. Only the
  newest release updates the Homebrew tap.
- Releases carry `SHA256SUMS`, and take their notes from this file.
- Workflows are read-only by default, with every action pinned to a commit.
  CI adds cargo-deny, an MSRV build and nextest, and Dependabot opens grouped
  updates weekly. The Nix build has its own workflow and a `flake.lock`.

## [0.14.4] - 2026-10-17

### Fixed

- Power: the battery shows on Macs whose `ioreg` output has tree-drawing
  prefixes (#37, by fucx).

## [0.14.3] - 2026-10-10

### Fixed

- Procs: the CPU HIST column no longer pushes the columns after it out of
  line as a process's history fills (#36).

## [0.14.2] - 2026-09-14

### Added

- A static `syswatch-linux-armv5te-static` asset for ARMv5TE boxes such as the
  Iomega ix2-dl, built with `cross` (#34, by baracudaz).

## [0.14.1] - 2026-09-12

### Fixed

- Insights in the live TUI report the real elapsed window (#33).

## [0.14.0] - 2026-09-12

### Added

- Reports without the TUI: `syswatch snapshot`, `insights`, `why` and `diff`
  (#30).
- Insights: a CPU baseline per machine, and a correlation card when two or
  more cards name the same process (#31).

## [0.13.0] - 2026-09-12

### Added

- Unattended recording with `--record --keep` (#29).

### Changed

- Replay streams from disk rather than loading the whole recording (#29).

## [0.12.0] - 2026-09-11

### Changed

- Recording format v3, about 110 times smaller. Version 1 and 2 recordings
  are refused with an error (#28).

### Fixed

- Insights report real elapsed time (#27).

## [0.11.0] - 2026-09-10

### Changed

- Idle CPU at 1 Hz fell from 12.9% of a core to about 4%. Collection runs on
  its own thread, the screen redraws only when something changed, and
  subprocesses have time limits, so a hung one can't freeze the UI (#26).

## [0.10.0] - 2026-08-22

### Fixed

- Linux: ZFS ARC counts as available memory, not used (#24, by ldmosquera).

## [0.9.0] - 2026-08-15

### Added

- The Dense view, six boxes on one screen, cycled with `V` (#25).

## [0.8.0] - 2026-08-11

### Added

- The Lite view, `--lite` or `L`: one 80x24 screen for one machine. Older
  recordings still replay.

## [0.7.7] - 2026-07-26

### Added

- A `terminal` theme that uses the terminal's own palette (#23). The
  btop-style fade is off under it.

### Fixed

- Nix: the version comes from `Cargo.toml`, and `target/` stays out of the
  source (#22).

## [0.7.6] - 2026-07-26

### Security

- Process names, mount paths, and device and service names are scrubbed of
  terminal escape sequences before they reach the screen, snapshots or
  recordings (#21).

### Fixed

- The filter works on Memory and Services, not just Procs (#20).

## [0.7.5] - 2026-07-19

### Fixed

- The footer shows `f` beside `/` as the filter key.

## [0.7.4] - 2026-07-19

### Fixed

- Procs: `f` also opens the filter, for keyboard layouts where `/` needs AltGr
  (#19).

## [0.7.3] - 2026-06-14

### Fixed

- GPU: live AMD telemetry on hosts with more than one GPU (#15).
- Insights: no disk-full card for read-only mounts (#9).
- Procs and Memory: column alignment, swap, the detail toggle, update rate
  and CPU history (#10, #11, #12, #13, #14).

## [0.7.2] - 2026-06-10

### Changed

- Per-process readouts mark estimates with `~`. `%MEM` replaces raw RSS, and
  the Memory tab breaks memory down into PSS, private, shared and swap on
  Linux and phys_footprint on macOS. macOS per-process network rates are
  measured.
- Recording format v2. Older recordings are refused with an error.

### Added

- Linux PSI stall readouts, real thread counts, per-process read and write
  rates, and insights for PSI stalls, private-memory leaks and energy use.

## [0.7.1] - 2026-06-03

### Fixed

- Linux: CPU temperature on AMD Ryzen, from the `k10temp` hwmon driver.
- Apple Silicon: the GPU VRAM graph is no longer blank (#7).

## [0.7.0] - 2026-05-31

### Added

- GPU utilization and VRAM graphs over time (#6).

### Fixed

- macOS: the network collector no longer forks a process storm, which cuts
  CPU use by about 35% (#5).

## [0.6.1] - 2026-05-24

### Fixed

- Platform sampling no longer blocks the TUI (#2, by chenqianhe).

## [0.6.0] - 2026-05-24

### Added

- An optional btop-style fade for charts and tables, in Settings.

## [0.5.0] - 2026-05-10

### Added

- Session recording to `.swr` files with `R`, and replay.

## [0.4.3] - 2026-05-10

### Added

- Procs sorts by GPU and by network rate.

## [0.4.2] - 2026-05-09

### Added

- Per-process GPU use on Linux, and a last-submitter hint on macOS.

## [0.4.1] - 2026-05-09

### Added

- Per-process network bandwidth on the Procs tab.

## [0.4.0] - 2026-05-09

### Added

- A help dialog on `?`, a filter on Procs, and a snapshot of the screen's
  data saved as JSON.

## [0.3.1] - 2026-05-06

### Added

- macOS: more Power tab detail from IOReport and SMC.

## [0.3.0] - 2026-05-06

### Added

- A settings popup. Theme, graph style, default tab and tick interval are
  saved to `~/.config/syswatch/config.toml`.

## [0.2.2] - 2026-05-05

### Added

- macOS: GPU temperature and power per rail, from IOReport and SMC.

## [0.2.1] - 2026-05-05

### Added

- More AMD GPU detail from sysfs, and NVIDIA support through NVML behind the
  `gpu-nvidia` feature.

## [0.2.0] - 2026-05-05

### Added

- btop-style charts and themes.

### Fixed

- Contrast in the light theme.

## [0.1.1] - 2026-05-02

### Added

- Live GPU utilization on Apple Silicon from `ioreg`, without sudo.

## [0.1.0] - 2026-05-02

### Added

- First release: twelve tabs (Overview, CPU, Memory, Disks, Filesystems,
  Procs, GPU, Power, Services, Network, Timeline and Insights).
