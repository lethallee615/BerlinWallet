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
//! All three backends are wired. They are not equivalent: bitcoind RPC cannot
//! do gap-limit discovery, so a restore there needs a birthday height (see
//! [`bitcoind`]).
//!
//! Chain-source parsing and construction live in [`config`]; this module keeps
//! the trait, the small chain types, and the error taxonomy.

use std::fmt;

use async_trait::async_trait;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{BlockHash, Transaction};

pub mod bitcoind;
pub mod config;
pub mod electrum;
pub mod esplora;
#[cfg(test)]
pub mod mock;
pub mod persist;

pub use config::{BitcoindAuth, ChainBackendConfig};

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

    /// Whether [`Self::full_scan`] consumes
    /// [`FullScanOptions::birthday_height`] as a starting height.
    ///
    /// Only block-walk backends do. Script-pubkey backends always scan from
    /// derivation index 0 and ignore it.
    fn full_scan_uses_birthday(&self) -> bool {
        false
    }
}
