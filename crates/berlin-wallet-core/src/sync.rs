//! Concurrent sync tasks (PLAN.md §5).
//!
//! `adoption_task` is real: it schedules the orphan-claim adoption pass. The
//! BDK chain and fedimint federation tasks remain scaffolds (no network I/O).

use std::time::Duration;

use fedimint_client::{Client, ClientHandleArc};
use fedimint_core::config::FederationId;

use crate::deposit;
use crate::journal::Journal;

/// Cadence of the orphan-claim adoption pass.
pub const ADOPTION_INTERVAL: Duration = Duration::from_secs(60);

/// Boot + every [`ADOPTION_INTERVAL`] (`tokio::time::interval`'s first tick is
/// immediate). Exits cleanly when `shutdown` changes to `true` or its sender is
/// dropped.
pub async fn adoption_task(
    client: ClientHandleArc,
    journal: Journal,
    federation_id: FederationId,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
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

pub async fn bdk_chain_task() -> anyhow::Result<()> {
    anyhow::bail!("bdk_chain_task: not implemented in the scaffold")
}

pub async fn fedimint_sync_task() -> anyhow::Result<()> {
    anyhow::bail!("fedimint_sync_task: not implemented in the scaffold")
}
