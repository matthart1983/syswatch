pub mod collector;
pub mod command;
#[cfg(target_os = "macos")]
pub mod disk_macos;
pub mod gpu;
#[cfg(target_os = "macos")]
pub mod macos_sampler;
pub mod model;
pub mod power;
pub mod proc_bandwidth;
pub mod proc_gpu;
pub mod proc_memory;
pub mod ring;
pub mod sanitize;
pub mod services;
pub mod worker;

// The UI talks to the worker; `Collector` itself is only constructed on
// that thread and in tests.
#[cfg_attr(not(test), allow(unused_imports))]
pub use collector::Collector;
pub use model::*;
pub use ring::Ring;
pub use worker::CollectorHandle;
