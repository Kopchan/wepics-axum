//! Process-wide libvips initialization.
//!
//! `vips_init` must run before any libvips call, but only once per process. The
//! binary initializes eagerly at startup for a clear error message; tests and
//! tools that only link the library reach this helper first instead.

use std::sync::OnceLock;

static STATE: OnceLock<Result<(), String>> = OnceLock::new();

/// Initializes libvips if needed. Errors are cached, so a broken installation
/// is reported once and not retried on every request.
pub fn ensure() -> Result<(), String> {
    STATE
        .get_or_init(|| {
            vips::init(Some("wepics-axum"))
                .map_err(|err| format!("cannot initialize libvips: {err}"))?;
            tune();
            Ok(())
        })
        .clone()
}

/// Keeps libvips from hoarding memory between requests.
///
/// The default operation cache holds up to ~100 MB of compute nodes, which made
/// a preview benchmark peak above the old renderer. Rendered previews are kept
/// on disk, so nothing is gained by keeping their pipeline graphs in RAM.
fn tune() {
    vips::set_max_operations(0);
    vips::set_max_mem_bytes(0);
}
