//! Shared crate for the native Arbiter spike — used by both the raw winit
//! binary (`main.rs`) and the Iced shell binary (`bin/iced_shell.rs`).

pub mod gpu;
pub mod session;
pub mod shell;
pub mod term;
