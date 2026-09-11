use anyhow::Result;
use clap::Parser;

mod app;
mod collect;
mod config;
mod insights;
mod recording;
mod snapshot;
mod tabs;
mod ui;

use config::SyswatchConfig;

#[derive(Parser, Debug)]
#[command(
    name = "syswatch",
    version,
    about = "Single-host system diagnostics TUI"
)]
struct Cli {
    /// Fast-loop tick in milliseconds. Overrides the saved config when supplied.
    #[arg(long)]
    tick: Option<u64>,

    /// Start on a specific tab (overview, cpu, memory, disks, fs, procs, gpu, power, services, net, timeline, insights).
    /// Overrides the saved config.default_tab when supplied.
    #[arg(long)]
    tab: Option<String>,

    /// Start in the Lite view: one 80×24 screen answering "why is this
    /// machine hot, slow, or loud?" with six keys. Toggle with `L` at any
    /// time — it is never selected automatically by terminal size.
    #[arg(long)]
    lite: bool,

    /// Start in the Dense view: every subsystem on one 130×44 screen, six
    /// boxes and zero chrome rows. Cycle views with `V` at any time — like
    /// Lite it is never selected automatically by terminal size.
    #[arg(long)]
    dense: bool,

    /// Replay a previously-recorded session (.swr file, written by
    /// pressing `R` during a live run). No live collection happens —
    /// arrow keys / Home / End scrub through the recorded ticks.
    #[arg(long, value_name = "PATH", conflicts_with = "record")]
    replay: Option<std::path::PathBuf>,

    /// Record continuously in the background, no TUI: sample on --tick
    /// (or the saved config's tick), write into rotating chunk files
    /// under the data directory, and prune chunks older than --keep as
    /// new ones start. Runs until interrupted (Ctrl-C / SIGTERM),
    /// which flushes the current chunk before exiting. Requires --keep.
    #[arg(long, requires = "keep")]
    record: bool,

    /// How much history an unattended --record run retains on disk.
    /// A number plus a unit: seconds/minutes/hours/days, e.g. "24h",
    /// "7d", "90m". Meaningless without --record.
    #[arg(long, value_name = "DURATION", value_parser = recording::parse_retention, requires = "record")]
    keep: Option<std::time::Duration>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = SyswatchConfig::load();

    // CLI --tick overrides config for this run only — it's mutated on the
    // in-memory copy so the running app reads it consistently. Saved
    // config on disk stays untouched until the user presses S in settings.
    if let Some(t) = cli.tick {
        cfg.tick_ms = t;
        cfg.validate();
    }

    // --record is a headless mode: no TUI, no theme, no tab -- dispatch
    // to it before any of that setup runs. `requires = "keep"` on the
    // clap arg means `cli.keep` is always Some here.
    if cli.record {
        let keep = cli.keep.expect("clap requires --keep with --record");
        let ring_dir = recording::ring_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine the data directory"))?;
        let tick = std::time::Duration::from_millis(cfg.tick_ms.clamp(100, 5000));

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let stop = std::sync::Arc::clone(&stop);
            ctrlc::set_handler(move || {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            })
            .map_err(|e| anyhow::anyhow!("could not install Ctrl-C handler: {e}"))?;
        }

        eprintln!(
            "syswatch: recording to {} (keeping {:?}, Ctrl-C to stop)",
            ring_dir.display(),
            keep
        );
        return recording::run_ring(&ring_dir, keep, recording::RING_CHUNK_DURATION, tick, &stop);
    }

    // Apply theme before any rendering so the first frame uses it.
    ui::theme::set_by_name(&cfg.theme);

    // CLI --tab wins; otherwise fall back to the persisted default.
    let start_tab = cli.tab.or_else(|| Some(cfg.default_tab.clone()));

    // Replay mode: a cheap pre-count sizes History's rings, then the
    // run loop streams the recording in tick by tick via
    // `RecordingReader` -- never a `Vec<Snapshot>` materializing the
    // whole file here just to hand it off. Default-tab override goes
    // to Timeline since that's where scrubbing lives.
    let replay = if let Some(path) = cli.replay {
        let total = recording::count(&path)
            .map_err(|e| anyhow::anyhow!("could not read recording {}: {}", path.display(), e))?;
        if total == 0 {
            return Err(anyhow::anyhow!(
                "recording {} contains no snapshots",
                path.display()
            ));
        }
        eprintln!(
            "syswatch: replaying {} snapshots from {}",
            total,
            path.display()
        );
        Some(app::ReplaySource { path, total })
    } else {
        None
    };

    app::run(app::Options {
        start_tab,
        config: cfg,
        replay,
        lite: cli.lite,
        dense: cli.dense,
    })
}
