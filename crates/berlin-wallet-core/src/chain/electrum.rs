//! Electrum backend (`bdk_electrum 0.23.2` → `electrum-client 0.24.1`).
//!
//! The upstream client is **blocking** (`bdk_electrum_client.rs:114,198`) and
//! has its own reconnect loop with `thread::sleep` up to 30 s
//! (`electrum-client/src/client.rs:32-100`). Every call therefore runs inside
//! `spawn_blocking` with `retry(0)` and a socket timeout, so this crate's retry
//! policy is the only one in play and a hung server cannot wedge an async
//! worker.
//!
//! The client is constructed per operation (no long-lived socket): opening a
//! wallet must not depend on the server being reachable. The cost is a TCP/TLS
//! handshake per sync tick and a cold `BdkElectrumClient` transaction/anchor
//! cache; §"follow-ups" in `ARCHITECTURE.md` records the persistent-client
//! alternative.

use std::time::Duration;

use async_trait::async_trait;
use bdk_electrum::BdkElectrumClient;
use bdk_electrum::electrum_client;
use bdk_electrum::electrum_client::ElectrumApi;
use bdk_wallet::{KeychainKind, Update, Wallet};
use bitcoin::{BlockHash, Transaction};

use super::{ChainBackend, ChainError, ChainKind, ChainTip, FullScanOptions};

/// Socket-level timeout so a wedged server eventually frees the blocking thread.
const SOCKET_TIMEOUT_SECS: u8 = 30;
/// Wall-clock bound for a single tip/hash/broadcast round trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Wall-clock bound for an incremental sync.
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);
/// Wall-clock bound for a restore full scan.
const FULL_SCAN_TIMEOUT: Duration = Duration::from_secs(600);
/// Scripts per batched Electrum request.
const BATCH_SIZE: usize = 100;

/// Electrum client wrapper. Holds configuration only; the socket is created per
/// operation.
pub struct ElectrumBackend {
    url: String,
    config: electrum_client::Config,
}

impl ElectrumBackend {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        let url = url.trim();
        anyhow::ensure!(!url.is_empty(), "electrum URL must not be empty");
        anyhow::ensure!(
            url.starts_with("tcp://") || url.starts_with("ssl://") || !url.contains("://"),
            "electrum URL must start with tcp:// or ssl:// (got `{url}`)"
        );
        // `retry(0)` disables the client's internal reconnect/sleep loop: the
        // wallet's own bounded retry (SYNC_MAX_ATTEMPTS) is the only policy.
        let config = electrum_client::ConfigBuilder::new()
            .timeout(Some(SOCKET_TIMEOUT_SECS))
            .retry(0)
            .build();
        Ok(Self {
            url: url.to_string(),
            config,
        })
    }
}

/// Open a client. Must only be called from inside `spawn_blocking`: it performs
/// the TCP/TLS handshake.
fn connect(
    url: &str,
    config: &electrum_client::Config,
) -> Result<BdkElectrumClient<electrum_client::Client>, electrum_client::Error> {
    let client = electrum_client::Client::from_config(url, config.clone())?;
    Ok(BdkElectrumClient::new(client))
}

/// IO errors and exhausted-all-attempts are retryable; protocol rejections and
/// malformed responses are not. The remaining variants (`JSON`, `Hex`,
/// `Bitcoin`, `InvalidResponse`, `MissingDomain`, …) are all local/parse
/// failures and never fix themselves.
fn classify(err: electrum_client::Error) -> ChainError {
    use electrum_client::Error;
    let transient = matches!(
        &err,
        Error::IOError(_)
            | Error::SharedIOError(_)
            | Error::AllAttemptsErrored(_)
            | Error::CouldntLockReader
            | Error::Mpsc
    );
    if transient {
        ChainError::transient(ChainKind::Electrum, anyhow::anyhow!(err))
    } else {
        ChainError::permanent(ChainKind::Electrum, anyhow::anyhow!(err))
    }
}

/// Servers report a re-broadcast as an error: `txn-already-known`,
/// `txn-already-in-mempool`, or `already in block chain`. A crash-retry that
/// re-broadcasts the same funding transaction must not fail.
fn is_already_known(err: &electrum_client::Error) -> bool {
    let text = match err {
        electrum_client::Error::Protocol(value) => value.to_string(),
        electrum_client::Error::Message(message) => message.clone(),
        _ => return false,
    };
    text.to_ascii_lowercase().contains("already")
}

/// Run blocking work off the async worker, with an optional wall-clock bound.
///
/// A timed-out blocking task keeps running until its socket timeout fires;
/// that is why `SOCKET_TIMEOUT_SECS` is always set.
async fn run_blocking<T, F>(timeout: Option<Duration>, work: F) -> Result<T, ChainError>
where
    F: FnOnce() -> Result<T, electrum_client::Error> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::task::spawn_blocking(work);
    let joined = match timeout {
        Some(limit) => match tokio::time::timeout(limit, handle).await {
            Ok(joined) => joined,
            Err(_) => {
                return Err(ChainError::transient(
                    ChainKind::Electrum,
                    anyhow::anyhow!("electrum request timed out"),
                ));
            }
        },
        None => handle.await,
    };
    match joined {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(classify(err)),
        Err(join_err) => Err(ChainError::permanent(
            ChainKind::Electrum,
            anyhow::anyhow!("electrum blocking task panicked: {join_err}"),
        )),
    }
}

fn apply(wallet: &mut Wallet, update: Update) -> Result<(), ChainError> {
    wallet
        .apply_update(update)
        .map_err(|err| ChainError::permanent(ChainKind::Electrum, anyhow::anyhow!(err)))
}

#[async_trait]
impl ChainBackend for ElectrumBackend {
    fn kind(&self) -> ChainKind {
        ChainKind::Electrum
    }

    fn describe(&self) -> String {
        format!("electrum: {}", self.url)
    }

    async fn tip(&self) -> Result<ChainTip, ChainError> {
        let url = self.url.clone();
        let config = self.config.clone();
        run_blocking(Some(REQUEST_TIMEOUT), move || {
            let client = connect(&url, &config)?;
            // `blockchain.headers.subscribe` returns the current tip header, so
            // the hash comes from the same response (no second round trip).
            let notification = client.inner.block_headers_subscribe()?;
            let height = u32::try_from(notification.height)
                .map_err(|_| electrum_client::Error::Message("tip height overflow".into()))?;
            Ok(ChainTip {
                height,
                hash: notification.header.block_hash(),
            })
        })
        .await
    }

    async fn block_hash_at(&self, height: u32) -> Result<Option<BlockHash>, ChainError> {
        let url = self.url.clone();
        let config = self.config.clone();
        // The caller only asks for heights at or below the remote tip. If the
        // tip shrinks in between, Electrum rejects the height and this surfaces
        // as a permanent error; the next sync tick re-runs the height check and
        // detects the (lower-tip) reorg from `tip()` alone.
        let height = usize::try_from(height).map_err(|_| {
            ChainError::permanent(ChainKind::Electrum, anyhow::anyhow!("height overflow"))
        })?;
        let hash = run_blocking(Some(REQUEST_TIMEOUT), move || {
            let client = connect(&url, &config)?;
            let header = client.inner.block_header(height)?;
            Ok(header.block_hash())
        })
        .await?;
        Ok(Some(hash))
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), ChainError> {
        let url = self.url.clone();
        let config = self.config.clone();
        let tx = tx.clone();
        run_blocking(Some(REQUEST_TIMEOUT), move || {
            let client = connect(&url, &config)?;
            match client.transaction_broadcast(&tx) {
                Ok(txid) => {
                    tracing::debug!(%txid, "electrum accepted the transaction");
                    Ok(())
                }
                Err(err) if is_already_known(&err) => {
                    tracing::info!(%err, "electrum reports the transaction is already known; treating broadcast as success");
                    Ok(())
                }
                Err(err) => Err(err),
            }
        })
        .await
    }

    async fn sync(&self, wallet: &mut Wallet) -> Result<(), ChainError> {
        let url = self.url.clone();
        let config = self.config.clone();
        let request = wallet.start_sync_with_revealed_spks().build();
        // `fetch_prev_txouts = true` matches Esplora, which inserts previous
        // outputs via `insert_prevouts`; without them `Wallet::calculate_fee`
        // reports `MissingTxOut` for inputs the wallet does not own.
        let response = run_blocking(Some(SYNC_TIMEOUT), move || {
            let client = connect(&url, &config)?;
            client.sync(request, BATCH_SIZE, true)
        })
        .await?;
        apply(wallet, Update::from(response))
    }

    async fn full_scan(
        &self,
        wallet: &mut Wallet,
        options: FullScanOptions,
    ) -> Result<Vec<(KeychainKind, u32)>, ChainError> {
        let url = self.url.clone();
        let config = self.config.clone();
        let stop_gap = options.stop_gap;
        let request = wallet.start_full_scan().build();
        let response = run_blocking(Some(FULL_SCAN_TIMEOUT), move || {
            let client = connect(&url, &config)?;
            client.full_scan(request, stop_gap, BATCH_SIZE, true)
        })
        .await?;
        let last_active = response
            .last_active_indices
            .iter()
            .map(|(keychain, index)| (*keychain, *index))
            .collect();
        apply(wallet, Update::from(response))?;
        Ok(last_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_known_messages_are_recognised() {
        let err = electrum_client::Error::Protocol(serde_json::json!({
            "code": 1,
            "message": "the transaction was rejected by network rules.\n\n[tc1] txn-already-known"
        }));
        assert!(is_already_known(&err));

        let err = electrum_client::Error::Message("already in block chain".to_string());
        assert!(is_already_known(&err));

        let err = electrum_client::Error::Message("insufficient fee".to_string());
        assert!(!is_already_known(&err));
    }

    #[test]
    fn io_errors_are_transient_and_protocol_errors_are_not() {
        let io = electrum_client::Error::IOError(std::io::Error::other("connection reset"));
        assert!(classify(io).is_transient());

        let protocol =
            electrum_client::Error::Protocol(serde_json::json!({"code": 1, "message": "nope"}));
        assert!(!classify(protocol).is_transient());
    }

    #[test]
    fn url_must_use_a_supported_scheme() {
        assert!(ElectrumBackend::connect("tcp://127.0.0.1:50001").is_ok());
        assert!(ElectrumBackend::connect("ssl://127.0.0.1:50002").is_ok());
        assert!(ElectrumBackend::connect("http://127.0.0.1:50001").is_err());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_transient_error() {
        // Port 1 on loopback is never listening, so the real client exercises
        // the connect path and the classifier that the retry loop depends on.
        let backend = ElectrumBackend::connect("tcp://127.0.0.1:1").expect("connect");
        let err = backend.tip().await.expect_err("unreachable server");
        assert!(err.is_transient(), "unexpected: {err:#}");
    }
}
