//! Esplora backend (`bdk_esplora 0.22.1` / `esplora-client 0.12.3`).
//!
//! Esplora is the only genuinely async backend: `EsploraAsyncExt::sync` /
//! `full_scan` (`bdk_esplora/src/async_ext.rs:47,33`) consume a BDK request and
//! return a response that converts into `bdk_wallet::Update`.

use std::time::Duration;

use async_trait::async_trait;
use bdk_esplora::EsploraAsyncExt;
use bdk_esplora::esplora_client;
use bdk_wallet::{KeychainKind, Update, Wallet};
use bitcoin::{BlockHash, Transaction};

use super::{ChainBackend, ChainError, ChainKind, ChainTip, FullScanOptions};

/// Bound each HTTP operation so a hung endpoint cannot wedge a sync task.
/// `esplora_client::Builder::timeout` is the socket bound; the `tokio::time`
/// bounds below cover a whole request round trip.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A restore scan walks historical gap limits; give it a much larger bound than
/// an incremental sync.
const FULL_SCAN_TIMEOUT: Duration = Duration::from_secs(600);
const PARALLEL_REQUESTS: usize = 4;
const SOCKET_TIMEOUT_SECS: u64 = 30;

/// Esplora HTTP client wrapper.
pub struct EsploraBackend {
    client: esplora_client::AsyncClient,
    url: String,
}

impl EsploraBackend {
    pub fn connect(url: &str) -> anyhow::Result<Self> {
        let url = url.trim();
        anyhow::ensure!(!url.is_empty(), "esplora URL must not be empty");
        let client = esplora_client::Builder::new(url)
            .timeout(SOCKET_TIMEOUT_SECS)
            .build_async()?;
        Ok(Self {
            client,
            url: url.to_string(),
        })
    }
}

/// Connection refused, DNS failure, and request timeouts are retryable.
/// Malformed bodies/parsing and HTTP status errors are surfaced normally.
pub(crate) fn is_transient(err: &esplora_client::Error) -> bool {
    match err {
        esplora_client::Error::Reqwest(err) => {
            err.is_connect() || err.is_timeout() || err.is_request()
        }
        _ => false,
    }
}

/// Esplora reports a re-broadcast as an HTTP error whose body says the
/// transaction is already in the mempool or already in the chain.
///
/// This is success for our purposes: `fund_deposit` can re-run after a crash and
/// rebuild the same transaction, and a duplicate broadcast must not look like a
/// funding failure.
pub(crate) fn is_already_known(err: &esplora_client::Error) -> bool {
    match err {
        esplora_client::Error::HttpResponse { message, .. } => {
            message.to_ascii_lowercase().contains("already")
        }
        _ => false,
    }
}

fn classify(err: esplora_client::Error) -> ChainError {
    if is_transient(&err) {
        ChainError::transient(ChainKind::Esplora, anyhow::anyhow!(err))
    } else {
        ChainError::permanent(ChainKind::Esplora, anyhow::anyhow!(err))
    }
}

fn apply(wallet: &mut Wallet, update: Update) -> Result<(), ChainError> {
    wallet
        .apply_update(update)
        .map_err(|err| ChainError::permanent(ChainKind::Esplora, anyhow::anyhow!(err)))
}

#[async_trait]
impl ChainBackend for EsploraBackend {
    fn kind(&self) -> ChainKind {
        ChainKind::Esplora
    }

    fn describe(&self) -> String {
        format!("esplora: {}", self.url)
    }

    async fn tip(&self) -> Result<ChainTip, ChainError> {
        let height = tokio::time::timeout(REQUEST_TIMEOUT, self.client.get_height())
            .await
            .map_err(|_| {
                ChainError::transient(ChainKind::Esplora, anyhow::anyhow!("esplora tip timed out"))
            })?
            .map_err(classify)?;
        let hash = tokio::time::timeout(REQUEST_TIMEOUT, self.client.get_tip_hash())
            .await
            .map_err(|_| {
                ChainError::transient(
                    ChainKind::Esplora,
                    anyhow::anyhow!("esplora tip hash timed out"),
                )
            })?
            .map_err(classify)?;
        Ok(ChainTip { height, hash })
    }

    async fn block_hash_at(&self, height: u32) -> Result<Option<BlockHash>, ChainError> {
        match tokio::time::timeout(REQUEST_TIMEOUT, self.client.get_block_hash(height)).await {
            Ok(Ok(hash)) => Ok(Some(hash)),
            // Esplora answers 404 for a height that is not on its best chain.
            Ok(Err(esplora_client::Error::HttpResponse { status: 404, .. })) => Ok(None),
            Ok(Err(err)) => Err(classify(err)),
            Err(_) => Err(ChainError::transient(
                ChainKind::Esplora,
                anyhow::anyhow!("esplora block hash lookup timed out"),
            )),
        }
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), ChainError> {
        match tokio::time::timeout(REQUEST_TIMEOUT, self.client.broadcast(tx)).await {
            Ok(Ok(_txid)) => Ok(()),
            Ok(Err(err)) if is_already_known(&err) => {
                tracing::info!(
                    %err,
                    "esplora reports the transaction is already known; treating broadcast as success"
                );
                Ok(())
            }
            Ok(Err(err)) => Err(classify(err)),
            Err(_) => Err(ChainError::transient(
                ChainKind::Esplora,
                anyhow::anyhow!("esplora broadcast timed out"),
            )),
        }
    }

    async fn sync(&self, wallet: &mut Wallet) -> Result<(), ChainError> {
        let request = wallet.start_sync_with_revealed_spks().build();
        let response = tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.client.sync(request, PARALLEL_REQUESTS),
        )
        .await
        .map_err(|_| {
            ChainError::transient(
                ChainKind::Esplora,
                anyhow::anyhow!("esplora sync timed out"),
            )
        })?
        .map_err(|err| classify(*err))?;
        apply(wallet, Update::from(response))
    }

    async fn full_scan(
        &self,
        wallet: &mut Wallet,
        options: FullScanOptions,
    ) -> Result<Vec<(KeychainKind, u32)>, ChainError> {
        let request = wallet.start_full_scan().build();
        let response = tokio::time::timeout(
            FULL_SCAN_TIMEOUT,
            self.client
                .full_scan(request, options.stop_gap, PARALLEL_REQUESTS),
        )
        .await
        .map_err(|_| {
            ChainError::transient(
                ChainKind::Esplora,
                anyhow::anyhow!("esplora full scan timed out"),
            )
        })?
        .map_err(|err| classify(*err))?;
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
    fn http_4xx_is_a_permanent_sync_error() {
        let error = esplora_client::Error::HttpResponse {
            status: 400,
            message: "bad request".to_owned(),
        };
        assert!(!is_transient(&error));
    }

    #[test]
    fn malformed_body_is_a_permanent_sync_error() {
        let error = esplora_client::Error::Parsing("not-a-number".parse::<u32>().unwrap_err());
        assert!(!is_transient(&error));
    }

    #[test]
    fn already_known_broadcast_is_not_a_failure() {
        let error = esplora_client::Error::HttpResponse {
            status: 400,
            message: "Transaction already in mempool".to_owned(),
        };
        assert!(is_already_known(&error));

        let error = esplora_client::Error::HttpResponse {
            status: 400,
            message: "bad-txns-inputs-missingorspent".to_owned(),
        };
        assert!(!is_already_known(&error));
    }
}
