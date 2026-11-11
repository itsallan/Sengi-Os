//! The system tray: icons of apps that keep running in the background.
//!
//! This is the groundwork: making what such an app sends safe to show.
//! The tray itself is not here yet, so most of it has no caller.
#![allow(dead_code)]

pub mod icons;
mod pixmap;
mod text;
