//! Neumorphic Direct2D interface for the click engine.
//!
//! `render::color` is platform-neutral so its WCAG contrast tests run on any
//! target. Everything that touches Direct2D is `#[cfg(windows)]`.

pub mod render;
pub mod widget;

#[cfg(windows)]
pub mod window;
