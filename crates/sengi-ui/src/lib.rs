//! The Sengi OS component library, compiled from `ui/`.
//!
//! Programs in this workspace import the `.slint` files directly from their
//! own UI sources. This crate exists so the library is built and checked on
//! its own, and exposes the gallery window used for visual review.

slint::include_modules!();
