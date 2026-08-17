//! Embeds the dashboard SPA static files into the binary at compile time.
//!
//! The SPA lives in `static/` (`index.html`, `app.js`, `app.css`, `sw.js`),
//! extracted from the deleted Python server's inline copy (792ce1f has the
//! history). Versioning is the `APP_VER` const inside `static/app.js` and the
//! `CACHE` name in `static/sw.js`, bumped together by hand — the server reads
//! the live value by parsing `app.js` (api/sse.rs), not from this crate.
//! (A `pub const APP_VER` used to live here claiming the sw.js CACHE derived
//! from it "so the two can never drift"; nothing referenced it and nothing
//! derived from it — an unimplemented promise, deleted per ethos rule 6.)

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
pub struct DashboardAssets;
