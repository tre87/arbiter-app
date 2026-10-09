//! Shared crate for the native Arbiter spike — used by both the raw winit
//! binary (`main.rs`) and the Iced shell binary (`bin/iced_shell.rs`).

pub mod about;
pub mod agent;
pub mod agent_screen;
pub mod agent_shim;
pub mod agent_state;
pub mod agent_watch;
pub mod agents_office;
pub mod attach;
pub mod conn;
pub mod debug;
pub mod editor;
pub mod explorer;
pub mod font;
pub mod git;
pub mod memdiag;
pub mod gpu;
pub mod highlight;
pub mod notify;
pub mod persist;
pub mod priority;
pub mod procscan;
pub mod raster;
pub mod remote;
pub mod session;
pub mod shell;
pub mod term;
pub mod term_signal;
pub mod wol;
