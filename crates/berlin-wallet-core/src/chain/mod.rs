//! Pluggable BDK chain backends.
//!
//! The trait is defined at the "bring the wallet's chain state up to date"
//! level rather than at a request/response level, because the upstream crates
//! have different shapes:
//!
//! - `bdk_esplora` and `bdk_electrum` are script-pubkey query clients. They
//!   consume a `bdk_core::spk_client::SyncRequest` / `FullScanRequest` and
//!   return a response that converts into a `bdk_wallet::Update`.
//! - `bdk_bitcoind_rpc` is a block emitter. It has no script-pubkey query at
//!   all; it walks blocks and the wallet applies them one at a time with
//!   `Wallet::apply_block_connected_to`.
//!
//! Both shapes are expressed as [`ChainBackend::sync`] and
//! [`ChainBackend::full_scan`] taking `&mut Wallet`. Persistence and the reorg
//! anchor check stay in [`crate::bdk::BdkWallet`], which is also why the
//! backend never persists: it only stages changes on the wallet.
//!
//! Esplora is wired today. Electrum and bitcoind RPC are the next backends; see
//! `ROADMAP.md`.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{BlockHash, Transaction};

pub mod esplora;
#[cfg(test)]
pub mod mock;

/// Gap limit used for a restore full scan. It is a floor for the script-pubkey
/// backends and the basis of the bounded range a block-walk backend derives.
pub const DEFAULT_STOP_GAP: usize = 20;

/// Which chain source is in use. Reported in errors and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainKind {
    Esplora,
    Electrum,
    Bitcoind,
}

impl ChainKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Esplora => "esplora",
            Self::Electrum => "electrum",
            Self::Bitcoind => "bitcoind",
        }
    }
}

impl fmt::Display for ChainKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The remote chain tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainTip {
    pub height: u32,
    pub hash: BlockHash,
}

/// Options for a restore full scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullScanOptions {
    pub stop_gap: usize,
    /// Dense block-walk backends need a starting height; script-pubkey backends
    /// ignore it.
    pub birthday_height: Option<u32>,
}

impl Default for FullScanOptions {
    fn default() -> Self {
        Self {
            stop_gap: DEFAULT_STOP_GAP,
            birthday_height: None,
        }
    }
}

/// A chain backend failure, already classified for the sync retry policy.
///
/// Classification is per backend by necessity: the upstream error types share
/// nothing, and the same condition (a dropped socket) is a different variant in
/// each. There is one enum, not one contract.
#[derive(Debug)]
pub enum ChainError {
    /// Retryable: connection refused/reset, timeout, transient server error.
    Transient {
        kind: ChainKind,
        source: anyhow::Error,
    },
    /// Not retryable: malformed response, authentication failure, rejection.
    Permanent {
        kind: ChainKind,
        source: anyhow::Error,
    },
    /// The backend cannot perform this operation at all.
    Unsupported { kind: ChainKind, what: &'static str },
}

impl ChainError {
    pub fn transient(kind: ChainKind, source: impl Into<anyhow::Error>) -> Self {
        Self::Transient {
            kind,
            source: source.into(),
        }
    }

    pub fn permanent(kind: ChainKind, source: impl Into<anyhow::Error>) -> Self {
        Self::Permanent {
            kind,
            source: source.into(),
        }
    }

    pub fn unsupported(kind: ChainKind, what: &'static str) -> Self {
        Self::Unsupported { kind, what }
    }

    pub fn kind(&self) -> ChainKind {
        match self {
            Self::Transient { kind, .. }
            | Self::Permanent { kind, .. }
            | Self::Unsupported { kind, .. } => *kind,
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }

    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Transient { kind, source } | Self::Permanent { kind, source } => {
                anyhow::anyhow!("{kind} chain backend: {source:#}")
            }
            Self::Unsupported { kind, what } => {
                anyhow::anyhow!("{kind} chain backend does not support {what}")
            }
        }
    }
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient { kind, source } => {
                write!(f, "{kind} chain transient error: {source:#}")
            }
            Self::Permanent { kind, source } => write!(f, "{kind} chain error: {source:#}"),
            Self::Unsupported { kind, what } => {
                write!(f, "{kind} chain backend does not support {what}")
            }
        }
    }
}

impl std::error::Error for ChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transient { source, .. } | Self::Permanent { source, .. } => {
                Some(source.as_ref())
            }
            Self::Unsupported { .. } => None,
        }
    }
}

/// One interchangeable chain data source.
///
/// Implementations must be `Send + Sync`, must not persist the wallet, and must
/// not own the BDK database: they stage changes on the passed [`Wallet`] and the
/// caller persists them.
#[async_trait]
pub trait ChainBackend: Send + Sync {
    fn kind(&self) -> ChainKind;

    /// Human-readable backend identity. Never contains credentials.
    fn describe(&self) -> String;

    /// Current remote tip. Used for the reorg ancestry check.
    async fn tip(&self) -> Result<ChainTip, ChainError>;

    /// The remote block hash at `height`.
    ///
    /// Returns `None` only when the backend can prove that the height is not on
    /// its best chain (for example, above the remote tip).
    async fn block_hash_at(&self, height: u32) -> Result<Option<BlockHash>, ChainError>;

    /// Broadcast a signed transaction.
    ///
    /// Implementations must treat "already known" as success so a crash-retry
    /// that re-broadcasts the same funding transaction does not fail.
    async fn broadcast(&self, tx: &Transaction) -> Result<(), ChainError>;

    /// Advance the wallet's chain state to the remote tip.
    async fn sync(&self, wallet: &mut Wallet) -> Result<(), ChainError>;

    /// Discover historical activity (restore/rebuild) and return the last active
    /// derivation index per keychain.
    async fn full_scan(
        &self,
        wallet: &mut Wallet,
        options: FullScanOptions,
    ) -> Result<Vec<(KeychainKind, u32)>, ChainError>;

    /// Backend-specific full-scan defaults.
    fn full_scan_options(&self) -> FullScanOptions {
        FullScanOptions::default()
    }
}

/// How to reach the chain, as supplied by the caller (CLI argument, FFI
/// parameter, or an environment variable).
///
/// Accepted today:
///
/// - `esplora:http://host:port`, or a bare `http://`/`https://` URL
///   (backwards compatible with the previous Esplora-only parameter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainBackendConfig {
    Esplora { url: String },
}

impl ChainBackendConfig {
    pub fn esplora(url: impl Into<String>) -> Self {
        Self::Esplora { url: url.into() }
    }

    pub fn kind(&self) -> ChainKind {
        match self {
            Self::Esplora { .. } => ChainKind::Esplora,
        }
    }

    /// Description suitable for logs and UI. Contains no credentials.
    pub fn describe(&self) -> String {
        match self {
            Self::Esplora { url } => format!("esplora: {url}"),
        }
    }

    /// Build the backend. This performs no network I/O, so opening a wallet
    /// works while the endpoint is unreachable.
    pub fn connect(&self) -> anyhow::Result<Arc<dyn ChainBackend>> {
        let backend: Arc<dyn ChainBackend> = match self {
            Self::Esplora { url } => Arc::new(esplora::EsploraBackend::connect(url)?),
        };
        Ok(backend)
    }
}

impl fmt::Display for ChainBackendConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl FromStr for ChainBackendConfig {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let source = input.trim();
        anyhow::ensure!(!source.is_empty(), "chain source must not be empty");

        if let Some(rest) = source.strip_prefix("esplora:") {
            let rest = rest.trim();
            anyhow::ensure!(!rest.is_empty(), "esplora URL must not be empty");
            return Ok(Self::esplora(rest));
        }
        if source.starts_with("http://") || source.starts_with("https://") {
            // Backwards compatible with the previous `esplora_url` parameter.
            return Ok(Self::esplora(source));
        }
        anyhow::bail!(
            "unrecognised chain source `{source}`: expected `esplora:http://…` \
             or a bare http(s) URL"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_http_url_stays_esplora() {
        let config: ChainBackendConfig = "http://127.0.0.1:3002".parse().expect("parse");
        assert_eq!(config.kind(), ChainKind::Esplora);
        assert_eq!(
            config,
            ChainBackendConfig::Esplora {
                url: "http://127.0.0.1:3002".to_string()
            }
        );
    }

    #[test]
    fn esplora_prefix_is_accepted() {
        let config: ChainBackendConfig = "esplora:https://esplora.example".parse().expect("parse");
        assert_eq!(config.kind(), ChainKind::Esplora);
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!("ftp://example".parse::<ChainBackendConfig>().is_err());
        assert!("".parse::<ChainBackendConfig>().is_err());
        assert!(
            "electrum:tcp://127.0.0.1:50001"
                .parse::<ChainBackendConfig>()
                .is_err()
        );
    }

    #[test]
    fn connect_does_no_network_io() {
        // Constructing the backend against an unreachable endpoint must succeed;
        // the HTTP client connects per request.
        let config: ChainBackendConfig = "http://127.0.0.1:1".parse().expect("parse");
        let backend = config.connect().expect("connect");
        assert_eq!(backend.kind(), config.kind());
        assert!(!backend.describe().is_empty());
    }
}
