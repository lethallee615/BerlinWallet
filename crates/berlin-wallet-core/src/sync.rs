//! Concurrent sync tasks (`docs/PLAN.md` §5).
//!
//! All long-running tasks take the same `watch::Receiver<bool>` shutdown
//! signal. The BDK task performs a partial Esplora sync over revealed script
//! pubkeys and persists the resulting checkpoint; the fedimint task keeps the
//! reconnect tasks alive and drains transient events; the adoption task
//! reconciles orphan claims.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fedimint_client::{Client, ClientHandleArc};
use fedimint_core::config::FederationId;
use fedimint_walletv2_client::WalletClientModule;
use tokio::sync::{Mutex, broadcast::error::TryRecvError, watch};

use crate::bdk::BdkWallet;
use crate::deposit;
use crate::journal::{DepositState, Journal};
use crate::ops::DepositId;
use crate::withdrawal;

/// Cadence of the orphan-claim adoption pass.
pub const ADOPTION_INTERVAL: Duration = Duration::from_secs(60);
/// Cadence of the BDK Esplora partial sync.
pub const BDK_SYNC_INTERVAL: Duration = Duration::from_secs(30);
/// Cadence of the fedimint event/reconnect task.
pub const FEDIMINT_SYNC_INTERVAL: Duration = Duration::from_secs(30);
/// Pause between completed observation passes for a funded deposit.
pub const DEPOSIT_WATCH_RETRY: Duration = Duration::from_secs(30);
/// How long a funded deposit may stay unclaimed before warning.
pub const DEPOSIT_STALL_WARN: Duration = Duration::from_secs(300);

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
                let client_ref: &Client = &client;
                if let Err(err) =
                    deposit::adopt_orphan_claims(client_ref, &journal, federation_id).await
                {
                    tracing::warn!(?err, "adoption pass failed; will retry next tick");
                }
                if let Err(err) =
                    deposit::reconcile_missing_net_credits(client_ref, &journal).await
                {
                    tracing::warn!(
                        ?err,
                        "deposit net-credit reconciliation failed; will retry next tick"
                    );
                }
                if let Err(err) =
                    withdrawal::adopt_orphan_sends(client_ref, &journal, federation_id).await
                {
                    tracing::warn!(
                        ?err,
                        "withdrawal Send adoption failed; will retry next tick"
                    );
                }
            }
        }
    }
}

/// Tracks in-process deposit observers so one deposit never gets two.
///
/// Observation is idempotent in the journal, but duplicate tasks would duplicate
/// polling and log lines. The set is per-process; [`Self::resume`] re-creates the
/// observers after a restart. Handles are kept so [`Self::wait_all`] can drain
/// them before a client handle is consumed (the FFI `Arc::try_unwrap` check).
#[derive(Clone, Default)]
pub struct DepositWatchers {
    state: Arc<StdMutex<WatcherState>>,
}

#[derive(Default)]
struct WatcherState {
    active: BTreeSet<DepositId>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl DepositWatchers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start one observer for `id`. Returns `false` when one is already
    /// running.
    pub fn spawn(
        &self,
        client: ClientHandleArc,
        journal: Journal,
        federation_id: FederationId,
        id: DepositId,
        shutdown: watch::Receiver<bool>,
    ) -> bool {
        let mut state = self.state.lock().expect("deposit watcher set poisoned");
        if !state.active.insert(id.clone()) {
            return false;
        }
        state.tasks.retain(|task| !task.is_finished());

        // The stall warning runs on its own timer. `watch_deposit` retries a
        // timed-out `await_receive` internally, so the observation task alone
        // would never reach a point where it could report a stall. It is
        // detached on purpose: it holds no client handle, and tying it to
        // `wait_all` would make every clean quit wait for its timer.
        {
            let journal = journal.clone();
            let id = id.clone();
            let mut shutdown = shutdown.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown.changed() => {}
                    _ = tokio::time::sleep(DEPOSIT_STALL_WARN) => {
                        if let Ok(Some(row)) = journal.deposit(&id).await
                            && matches!(
                                row.state,
                                DepositState::AwaitingAddress | DepositState::AddressShown
                            )
                            && row.funding_txid.is_some()
                        {
                            tracing::warn!(
                                deposit = %row.id.0,
                                address = row.address.as_deref().unwrap_or("<none>"),
                                amount_sat = row.amount.to_sat(),
                                funding_txid = %row
                                    .funding_txid
                                    .map(|txid| txid.to_string())
                                    .unwrap_or_default(),
                                "funded deposit still has no federation receive claim after {}s; \
                                 the federation may not observe the funding transaction, or the \
                                 amount may not cover the claim fee",
                                DEPOSIT_STALL_WARN.as_secs()
                            );
                        }
                    }
                }
            });
        }

        let shared = self.state.clone();
        state.tasks.push(tokio::spawn(async move {
            if let Err(err) =
                watch_funded_deposit(&client, &journal, federation_id, &id, shutdown).await
            {
                tracing::warn!(
                    deposit = %id.0,
                    ?err,
                    "deposit observer stopped with an error; the adoption pass will still reconcile a claim"
                );
            }
            if let Ok(mut state) = shared.lock() {
                state.active.remove(&id);
            }
        }));
        true
    }

    /// Stop and drain every observer. The shutdown signal is sent by the
    /// caller; this waits briefly for cooperative exit and aborts anything that
    /// is still running so it releases its client handle.
    pub async fn wait_all(&self) {
        let tasks = {
            let mut state = self.state.lock().expect("deposit watcher set poisoned");
            state.active.clear();
            std::mem::take(&mut state.tasks)
        };
        for mut task in tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                // Wait for cancellation so the task drops its client handle
                // before the caller inspects the reference count.
                let _ = task.await;
            }
        }
    }

    /// Restart observers for deposits funded before this process started.
    /// Returns how many were started.
    pub async fn resume(
        &self,
        client: &ClientHandleArc,
        journal: &Journal,
        federation_id: FederationId,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<usize> {
        let mut started = 0;
        for row in journal
            .deposits_in(&[DepositState::AwaitingAddress, DepositState::AddressShown])
            .await?
        {
            if row.federation_id != federation_id || row.funding_txid.is_none() {
                continue;
            }
            if self.spawn(
                client.clone(),
                journal.clone(),
                federation_id,
                row.id,
                shutdown.clone(),
            ) {
                started += 1;
            }
        }
        Ok(started)
    }
}

async fn watch_funded_deposit(
    client: &ClientHandleArc,
    journal: &Journal,
    federation_id: FederationId,
    id: &DepositId,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let module = client.get_first_module_arc::<WalletClientModule>()?;
    let client_ref: &Client = client;

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let Some(row) = journal.deposit(id).await? else {
            return Ok(());
        };
        if !matches!(
            row.state,
            DepositState::AwaitingAddress | DepositState::AddressShown
        ) || row.funding_txid.is_none()
        {
            return Ok(());
        }
        // On a restart, resume past events already consumed for this row.
        let Some(position) = row.next_position.or(row.position) else {
            return Ok(());
        };

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            result = deposit::watch_deposit(
                client_ref,
                journal,
                &module,
                id,
                federation_id,
                position,
            ) => {
                if let Err(err) = result {
                    tracing::warn!(
                        deposit = %id.0,
                        ?err,
                        "deposit observation pass failed; will retry"
                    );
                }
            }
        }

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(DEPOSIT_WATCH_RETRY) => {}
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
pub async fn fedimint_sync_task(
    client: ClientHandleArc,
    journal: Journal,
    mut shutdown: watch::Receiver<bool>,
) {
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
                if let Err(err) = withdrawal::reconcile_submitted(&client, &journal).await {
                    tracing::warn!(?err, "withdrawal reconciliation pass failed");
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
