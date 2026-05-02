# Launch drafts — never auto-posted

Three variants. Pick one, edit, fire from your account. None of this is sent for you.

---

## Variant A — short announcement (X / LinkedIn)

> Shipped **SysWatch** — a single-host system diagnostics TUI. Sibling to netwatch.
>
> Twelve tabs covering everything that runs on one box: CPU, memory, disks, FS, procs, GPU, power, services, network — plus a Timeline scrubber that rewinds the entire app and an Insights tab that calls out swap thrash, runaway procs, disk full, etc. in plain English.
>
> macOS + Linux. Read-only by design.
>
> github.com/matthart1983/syswatch

[attach demo.gif]

---

## Variant B — technical thread opener (X)

> The terminal you open when something feels off — before you reach for htop, iostat, nettop, powermetrics, and a notebook full of one-liners.
>
> Where htop shows you *what's running*, SysWatch shows you *what's happening* — and tells you why in plain English when something's anomalous.
>
> 🧵

Then thread:

> 1/ **Twelve tabs.** Overview, CPU, Memory, Disks, FS, Procs, GPU, Power, Services, Net, Timeline, Insights. Numbered 1-9, 0, -, +. Same chrome and palette as netwatch.

> 2/ **Insights** is the differentiator. Heuristics over a rolling session window: swap thrash, runaway processes (per-pid CPU EWMA, not transient spikes), disk full, memory pressure, high load, zombie parties. Each card names a suggested tab.

> 3/ **Timeline scrubs the whole app.** Press ← and every tab — Procs, Memory, Net, all of them — silently shows that historical state. The session ring is the foundation Snapshot/Diff and Recording will sit on (v0.2).

> 4/ **Honest about platform limits.** Where data needs sudo (powermetrics for fans + per-component power on macOS, GPU live util on Apple Silicon), the tab shows what we *can* get for free and a one-line note about what's gated. Nothing is faked, nothing prompts.

> 5/ macOS + Linux. Cross-platform collection via sysinfo. Net interface counters and aggregate disk IO route through netwatch-sdk so SysWatch and the netwatch agent share the same parsers.
>
> Read-only by design — we don't kill, renice, unmount, or restart.

> 6/ Source: github.com/matthart1983/syswatch
> Crate: crates.io/crates/syswatch
> Brew: brew install matthart1983/tap/syswatch

---

## Variant C — Reddit (r/commandline / r/rust)

**Title:** I built SysWatch — single-host diagnostics TUI in Rust. Sibling to netwatch. Twelve tabs, plain-English insights, session scrubber.

**Body:**

After shipping netwatch (network diagnostics TUI, ~1300 stars), I kept hitting the same workflow problem: when something felt off on a box, I'd cycle through htop → iostat → nettop → powermetrics → launchctl, copying numbers into a notebook.

SysWatch is what I wanted instead: one tool, twelve tabs, same chrome as netwatch. macOS + Linux.

**What's distinctive vs. htop/btop/glances:**

- **Insights tab.** Heuristic anomaly detection — swap thrash, runaway processes (per-pid CPU EWMA), disk full, memory pressure, high load. Plain-English cards with a suggested tab. The Overview's bottom strip and the tab bar's [+] badge keep them in sight from anywhere.
- **Session-wide scrubbing.** Timeline's ←/→ rewinds the entire app — every panel transparently shows historical state. Snapshot/Diff lands in v0.2 on this foundation.
- **Honest about platform limits.** Where data needs sudo (powermetrics, IOReport on Apple Silicon), the tab shows what's free and a one-line note about what's gated. Nothing is faked.

**What's deferred:**

- Snapshot/Diff/Profiles/Recording — footer hotkeys present, v0.2.
- macOS fans + per-component power (need powermetrics).
- NVIDIA live GPU stats (behind `gpu-nvidia` cargo feature).
- SMART (behind `smart` cargo feature).

Read-only by design. We never kill, renice, unmount, or restart.

Repo: https://github.com/matthart1983/syswatch

Feedback welcome — especially "this would be more useful if it also surfaced X."
