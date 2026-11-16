//! The Sengi OS login screen, as a library.
//!
//! The program is `src/main.rs`. What is here is kept apart so it can be
//! tested without a compositor and without greetd:
//!
//! * [`ipc`]: greetd's protocol, requests out and replies in.
//! * [`login`]: one login after another, spoken to greetd on a thread.
//! * [`users`]: who can log in here.
//! * [`screen`]: the login screen's states and what it shows in each.
//! * [`power`]: restarting and shutting down.
//!
//! The password as it is typed, the field's texts and the waits after
//! wrong passwords are the lock screen's, from the `sengi-lock` crate.

pub mod cli;
pub mod ipc;
pub mod login;
pub mod power;
pub mod screen;
pub mod users;
