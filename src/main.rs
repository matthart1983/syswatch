use anyhow::Result;
use clap::Parser;

mod app;
mod collect;
mod tabs;
mod ui;

#[derive(Parser, Debug)]
#[command(name = "syswatch", version, about = "Single-host system diagnostics TUI")]
struct Cli {
    /// Fast-loop tick in milliseconds.
    #[arg(long, default_value_t = 1000)]
    tick: u64,

    /// Start on a specific tab (overview, cpu, memory, disks, fs, procs, gpu, power, services, net, timeline, insights).
    #[arg(long)]
    tab: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    app::run(app::Options {
        tick_ms: cli.tick,
        start_tab: cli.tab,
    })
}
