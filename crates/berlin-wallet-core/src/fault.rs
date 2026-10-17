//! Debug-only fault injection for crash/restart tests.
//!
//! Set `BERLIN_WALLET_CRASH_POINT` to a named point; debug builds abort at
//! that point. Release builds compile this to a no-op.

pub(crate) fn crash_point(point: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("BERLIN_WALLET_CRASH_POINT").ok().as_deref() == Some(point) {
        eprintln!("fault injection: aborting at {point}");
        std::process::abort();
    }
}
