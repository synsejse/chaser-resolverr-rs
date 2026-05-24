//! Vendored chaser-cf library — Cloudflare bypass via headful Chrome.
//!
//! Two local patches vs upstream:
//!   * `wait_for_clearance` bails out early when the page is not a CF
//!     challenge (saves ~24s on non-CF URLs like Prowlarr's `/v1/ping`).
//!   * `get_source` returns the navigation HTTP status alongside the HTML
//!     so callers can stop faking 200.

pub mod core;
pub mod error;
pub mod models;

pub use core::{ChaserCF, ChaserConfig};
pub use error::ChaserError;
pub use models::{Cookie, Profile, ProxyConfig, SourceResponse, WafSession};
