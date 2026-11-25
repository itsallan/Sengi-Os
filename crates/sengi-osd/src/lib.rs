//! sengi-osd: the indicator that shows a level for a moment after a volume
//! or brightness key.
//!
//! The logic is here, so it can be tested; `main.rs` puts the panel on a
//! layer-shell surface. Nothing runs between key presses: the first press
//! starts the program, the presses that follow while it is open are handed
//! to it through a socket, and it ends a moment after the last.

pub mod cli;
pub mod instance;
pub mod level;
pub mod ui;
