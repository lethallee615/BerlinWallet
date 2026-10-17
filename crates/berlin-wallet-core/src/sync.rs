//! Concurrent sync tasks (PLAN.md §5).
//!
//! All long-running tasks take the same `watch::Receiver<bool>` shutdown
//! signal. The BDK task performs a partial Esplora sync over revealed script
//! pubkeys and persists the resulting checkpoint; the fedimint task keeps the
//! reconnect tasks alive and drains transient events; the adoption task
//! reconciles orphan claims.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fedimint_client::{Client, ClientHandleArc};
use fedimint_core::config::FederationId;
use tokio::sync::{Mutex, broadcast::error::TryRecvError, watch};

use crate::bdk::BdkWallet;
use crate::deposit;
use crate::journal::Journal;

/// Cadence of the orphan-claim adoption pass.
pub const ADOPTION_INTERVAL: Duration = Duration::from_secs(60);
/// Cadence of the BDK Esplora partial sync.
pub const BDK_SYNC_INTERVAL: Duration = Duration::from_secs(30);
/// Cadence of the fedimint event/reconnect task.
pub const FEDIMINT_SYNC_INTERVAL: Duration = Duration::from_secs(30);

/// Boot + every [`ADOPTION_INTERVAL`] (`tokio::time::interval`'s first tick is
/// immediate). Exits cleanly when `shutdown` changes to `true` or its sender is
/// dropped.
pub async fn adoption_task(
    client: ClientHandleArc,
    journal: Journal,
    federation_id: FederationId,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(ADOPTION_INTERVAL);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = ticker.tick() => {
                let client_ref: &Client = &**client;
                if let Err(err) =
                    deposit::adopt_orphan_claims(client_ref, &journal, federation_id).await
                {
                    tracing::warn!(?err, "adoption pass failed; will retry next tick");
                }
            }
        }
    }
}

/// Partial BDK sync loop. The first pass runs at boot; later passes use a
/// bounded interval with jitter.
pub async fn bdk_chain_task(wallet: Arc<Mutex<BdkWallet>>, mut shutdown: watch::Receiver<bool>) {
    let mut delay = Duration::ZERO;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(delay) => {
                let result = {
                    let mut wallet = wallet.lock().await;
                    wallet.sync().await
                };
                if let Err(err) = result {
                    tracing::warn!(?err, "BDK chain sync failed; will retry");
                }
                delay = jittered(BDK_SYNC_INTERVAL);
            }
        }
    }
}

/// Fedimint keepalive/event task. `Client::start_executor()` is already called
/// by `client::build_client`; this task starts the reconnect loop and drains
/// the transient event stream so slow consumers cannot block it.
pub async fn fedimint_sync_task(client: ClientHandleArc, mut shutdown: watch::Receiver<bool>) {
    client.spawn_federation_reconnect();
    let mut events = client.get_event_log_transient_receiver();
    let mut delay = Duration::ZERO;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(delay) => {
                loop {
                    match events.try_recv() {
                        Ok(event) => {
                            tracing::debug!(?event, "fedimint transient event");
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "fedimint event receiver lagged");
                        }
                        Err(TryRecvError::Closed) => return,
                    }
                }
                delay = jittered(FEDIMINT_SYNC_INTERVAL);
            }
        }
    }
}

/// `base` ± up to 10%, derived from the wall clock (no extra RNG dependency).
fn jittered(base: Duration) -> Duration {
    let base_ms = base.as_millis() as u64;
    let spread = (base_ms / 10).max(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let delta = nanos % (spread.saturating_mul(2) + 1);
    Duration::from_millis(base_ms.saturating_sub(spread) + delta)
}
