<p align="center">
  <h1 align="center">SysWatch</h1>
  <p align="center">
    <strong>Single-host system diagnostics in your terminal. The terminal you open when something feels off — before you reach for htop, iostat, nettop, powermetrics, and a notebook full of one-liners.</strong>
  </p>
  <p align="center">
    <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue" alt="Platform">
    <img src="https://img.shields.io/badge/license-MIT-green" alt="License">
    <img src="https://img.shields.io/badge/status-v0.1-yellow" alt="Status">
  </p>
</p>

<p align="center">
  <em>Sibling to <a href="https://github.com/matthart1983/netwatch">NetWatch</a>. Same chrome. Same palette. Twelve tabs covering everything that runs on one box.</em>
</p>

<p align="center">
  <img src="demo.gif" alt="SysWatch — Overview, CPU, Memory, Procs, Power, Services, Timeline, Insights" width="800">
</p>

---

## What it shows

| # | Tab | Replaces |
|---|---|---|
| 1 | Overview | dashboard view of all subsystems |
| 2 | CPU | `htop` CPU panel, `top -d`, `mpstat` |
| 3 | Memory | `free`, `vm_stat`, `htop` mem panel |
| 4 | Disks | `iostat`, `iotop` (aggregate) |
| 5 | Filesystems | `df -h`, `df -i`, `mount` |
| 6 | Procs | `htop`, `ps auxf`, `pstree` |
| 7 | GPU | `system_profiler SPDisplaysDataType` / `/sys/class/drm` |
| 8 | Power | `pmset`, `ioreg AppleSmartBattery` / `/sys/class/power_supply` |
| 9 | Services | `launchctl list` / `systemctl list-units` |
| 0 | Net | `nettop`, `iftop` |
| - | Timeline | (no equivalent — session log + scrubber) |
| + | Insights | (no equivalent — plain-English anomaly cards) |

Where `htop` shows you *what's running*, SysWatch shows you *what's happening* — across CPU, memory, IO, GPU, power, services — and tells you why in plain English when something's anomalous.

## Install

```bash
git clone https://github.com/matthart1983/syswatch.git && cd syswatch
cargo build --release
./target/release/syswatch
```

**Prerequisites:** Rust 1.75+. No system dependencies on Linux. macOS links against the system frameworks.

> Crates.io / Homebrew / pre-built binaries land with v0.1 release.

## Usage

```bash
syswatch                       # default 1Hz tick
syswatch --tick 500            # 2Hz
syswatch --tab procs           # boot straight into a tab
```

### Keys

```text
1 2 3 4 5 6 7 8 9   →  Overview / CPU / Mem / Disks / FS / Procs / GPU / Power / Services
0 - +               →  Net / Timeline / Insights
Tab / Shift-Tab     →  Cycle tabs
↑ / ↓               →  Select row (Procs, Services)
s                   →  Cycle sort (Procs, Services)
← / →               →  Scrub session backward / forward
Home / End          →  Oldest sample / live
p                   →  Pause
q / Ctrl-C          →  Quit
```

## What's distinctive

**Insights tab.** Heuristic anomaly detection over the rolling session — swap thrash, runaway processes, disk full, memory pressure, high load, zombie parties — surfaced as plain-English cards with a suggested tab. The Overview's bottom strip and the tab bar's `[+]` badge keep them in sight from anywhere.

**Session-wide scrubbing.** The Timeline tab's `←/→` rewinds the entire app — every panel transparently shows historical state. The session ring is the foundation Snapshot/Diff and Recording will sit on (v0.2).

**Honest about platform limits.** Where data needs sudo (`powermetrics` for fans, per-component power, GPU util on Apple Silicon) the tab shows what we *can* get for free and a one-line note about what's gated. Nothing is faked, nothing prompts.

## Anti-goals

- **Not multi-host.** For fleet view, use NetWatch's web dashboard.
- **Not a daemon.** No long-running collector, no Prometheus push. The session is the database.
- **Not interactive remediation.** Read-only, deliberately. We don't kill, renice, unmount, or restart.
- **Not a logging product.** We surface OOM kills as a *signal* in Memory; we are not a log search UI.
- **Not pretty charts for screenshots.** Block sparklines, real numbers, no smooth curves, no themes-of-the-week.

## v0.1 scope

All twelve tabs render real data on macOS and Linux. Cross-platform collection via `sysinfo`. Net interface counters and aggregate disk IO route through [`netwatch-sdk`](https://github.com/matthart1983/netwatch-sdk) so SysWatch and the NetWatch agent share a single source of truth for those parsers.

**Deferred to v0.2** — Snapshot+Diff (footer S/D), Profiles (P), Recording/Replay (R), Settings (`,`), Help (`?`), filter (`/`).

**Deferred behind sudo** — fans, per-component power, GPU live util on Apple Silicon (all need `powermetrics`); macOS thermal zone temps (need IOReport private FFI). Linux gets these for free via sysfs.

**Deferred behind features** — NVIDIA live GPU stats (`gpu-nvidia` cargo feature, `nvml-wrapper`), SMART disk health (`smart` cargo feature, `smartctl --json`).

## Architecture

```text
src/
├── main.rs              CLI + entry
├── app.rs               Event loop, tab state, scrub plumbing
├── collect/             One Collector per subsystem; Snapshot the wire format
│   ├── collector.rs     sysinfo-backed CPU/Mem/Procs + dispatch
│   ├── gpu.rs           system_profiler / sysfs DRM
│   ├── power.rs         ioreg / pmset / sysfs power_supply
│   ├── services.rs      launchctl / systemctl
│   └── ring.rs          Bounded history + nth_back for scrubbing
├── insights/            Pure functions over (History, &Snapshot)
├── tabs/                One file per tab; thin renderers over the model
└── ui/
    ├── chrome.rs        Header, tab bar, footer
    ├── palette.rs       Single source of color truth
    └── widgets.rs       block_bar, sparkline, panel
```

Refresh model: 1 Hz fast loop for CPU/Mem/Procs/Net/IO; slow loops at 5 s for Power/Services (subprocess-heavy on macOS). The UI redraws on tick or keypress; CPU budget target is < 0.5% at idle.

## License

MIT.
