//! Debug-only fault injection for crash/restart tests.
//!
//! Set `BERLIN_WALLET_CRASH_POINT` to a named point; debug builds abort at
//! that point. Release builds compile this to a no-op.
//!
//! Flow hooks:
//!
//! - `after_deposit_address` — after the deposit address is persisted.
//! - `after_deposit_broadcast` — after the funding txid/outpoint are recorded.
//! - `after_deposit_done` — after a claim closes the deposit row.
//! - `after_send` — after `send()` returns, before the operation id is persisted.
//! - `after_broadcast` — after the final send state and change wait, before
//!   the row is marked `Broadcast`.
//!
//! The deterministic CI matrix lives in `crash_matrix.rs`. The window between
//! a real network broadcast and the journal/BDK persistence is devimint-only;
//! see `crash_matrix::DEVIMINT_ONLY_WINDOWS`.

pub(crate) fn crash_point(point: &str) {
    #[cfg(debug_assertions)]
    {
        if std::env::var("BERLIN_WALLET_CRASH_POINT").ok().as_deref() == Some(point) {
            eprintln!("fault injection: aborting at {point}");
            std::process::abort();
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = point;
}
