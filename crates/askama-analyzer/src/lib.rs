//! The analysis behind `askama-analyzer`: which items derive `Template` in which crates, what
//! askama generates for each, and the errors that come back, in template positions.
//!
//! [`server`] serves it over the Language Server Protocol and [`check`] prints it, so the
//! editor and the terminal report the same things.

pub mod analysis;
pub mod check;
pub mod features;
pub mod generator;
pub mod index;
pub mod overlay;
pub mod project;
pub mod ra;
pub mod server;
pub mod text;
pub mod vfs;
pub mod workspace;
