//! Spoor keeps a logged-in browser session per website and lets scripts run
//! inside it. See `README.md` for the model; [`runtime`] is the core.

pub mod browser_util;
pub mod cache_dir;
pub mod capture;
pub mod cli;
pub mod client;
pub mod inspect;
pub mod log;
pub mod runtime;
pub mod server;
pub mod session;
pub mod site;
