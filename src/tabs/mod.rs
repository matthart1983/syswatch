use ratatui::{layout::Rect, Frame};

use crate::app::{App, Snapshot, TabId};

mod cpu;
mod disks;
mod fs;
mod gpu;
mod insights;
mod memory;
mod net;
mod overview;
mod power;
pub mod procs;
pub mod services;
mod timeline;

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    match app.active {
        TabId::Overview => overview::draw(f, area, app, snap),
        TabId::Cpu => cpu::draw(f, area, app, snap),
        TabId::Memory => memory::draw(f, area, app, snap),
        TabId::Net => net::draw(f, area, app, snap),
        TabId::Disks => disks::draw(f, area, app, snap),
        TabId::Fs => fs::draw(f, area, app, snap),
        TabId::Procs => procs::draw(f, area, app, snap),
        TabId::Gpu => gpu::draw(f, area, app, snap),
        TabId::Power => power::draw(f, area, app, snap),
        TabId::Services => services::draw(f, area, app, snap),
        TabId::Timeline => timeline::draw(f, area, app, snap),
        TabId::Insights => insights::draw(f, area, app, snap),
    }
}
