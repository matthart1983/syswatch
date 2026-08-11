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

    /// Replay a previously-recorded session (.swr file, written by
    /// pressing `R` during a live run). No live collection happens —
    /// arrow keys / Home / End scrub through the recorded ticks.
    #[arg(long, value_name = "PATH")]
    replay: Option<std::path::PathBuf>,
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

    // Apply theme before any rendering so the first frame uses it.
    ui::theme::set_by_name(&cfg.theme);

    // CLI --tab wins; otherwise fall back to the persisted default.
    let start_tab = cli.tab.or_else(|| Some(cfg.default_tab.clone()));

    // Replay mode: load the recording and hand it to the run loop
    // instead of opening a live Collector. Default-tab override goes
    // to Timeline since that's where scrubbing lives.
    let replay = if let Some(path) = cli.replay {
        let snaps = recording::read(&path)
            .map_err(|e| anyhow::anyhow!("could not read recording {}: {}", path.display(), e))?;
        if snaps.is_empty() {
            return Err(anyhow::anyhow!(
                "recording {} contains no snapshots",
                path.display()
            ));
        }
        eprintln!(
            "syswatch: replaying {} snapshots from {}",
            snaps.len(),
            path.display()
        );
        Some(snaps)
    } else {
        None
    };

    app::run(app::Options {
        start_tab,
        config: cfg,
        replay,
        lite: cli.lite,
    })
}
