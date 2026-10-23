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

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{BlockHash, Transaction};

pub mod bitcoind;
pub mod electrum;
pub mod esplora;
#[cfg(test)]
pub mod mock;
pub mod persist;

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
/// Accepted:
///
/// - `esplora:http://host:port`, or a bare `http://`/`https://` URL
///   (backwards compatible with the previous Esplora-only parameter).
/// - `electrum:tcp://host:50001`, `electrum:ssl://host:50002`, or a bare
///   `tcp://`/`ssl://` URL.
/// - `bitcoind:http://[user:pass@]host:8332[?cookie=/path/.cookie][&birthday=850000]`.
///
/// A bitcoind cookie file takes precedence over URL credentials. Credentials in
/// the URL are removed from the stored URL, so they never reach logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainBackendConfig {
    Esplora {
        url: String,
    },
    Electrum {
        url: String,
    },
    Bitcoind {
        url: String,
        auth: BitcoindAuth,
        birthday_height: Option<u32>,
    },
}

/// bitcoind JSON-RPC authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitcoindAuth {
    None,
    UserPass { user: String, password: String },
    CookieFile(PathBuf),
}

impl ChainBackendConfig {
    pub fn esplora(url: impl Into<String>) -> Self {
        Self::Esplora { url: url.into() }
    }

    pub fn electrum(url: impl Into<String>) -> Self {
        Self::Electrum { url: url.into() }
    }

    pub fn bitcoind(
        url: impl Into<String>,
        auth: BitcoindAuth,
        birthday_height: Option<u32>,
    ) -> Self {
        Self::Bitcoind {
            url: url.into(),
            auth,
            birthday_height,
        }
    }

    pub fn kind(&self) -> ChainKind {
        match self {
            Self::Esplora { .. } => ChainKind::Esplora,
            Self::Electrum { .. } => ChainKind::Electrum,
            Self::Bitcoind { .. } => ChainKind::Bitcoind,
        }
    }

    /// Description suitable for logs and UI. Contains no credentials.
    pub fn describe(&self) -> String {
        match self {
            Self::Esplora { url } => format!("esplora: {url}"),
            Self::Electrum { url } => format!("electrum: {url}"),
            Self::Bitcoind { url, auth, .. } => {
                let auth = match auth {
                    BitcoindAuth::None => "no auth".to_string(),
                    BitcoindAuth::UserPass { user, .. } => format!("rpc user {user}"),
                    BitcoindAuth::CookieFile(path) => format!("cookie {}", path.display()),
                };
                format!("bitcoind: {url} ({auth})")
            }
        }
    }

    /// Round-trippable form for persistence. Unlike [`Self::describe`] this is
    /// fed back to [`FromStr`], so it carries no cosmetic spaces.
    pub fn canonical(&self) -> String {
        match self {
            Self::Esplora { url } => format!("esplora:{url}"),
            Self::Electrum { url } => format!("electrum:{url}"),
            Self::Bitcoind {
                url,
                auth,
                birthday_height,
            } => {
                let mut options: Vec<String> = Vec::new();
                let authority = match auth {
                    BitcoindAuth::None => url.clone(),
                    BitcoindAuth::UserPass { user, password } => match url.split_once("://") {
                        Some((scheme, rest)) => format!("{scheme}://{user}:{password}@{rest}"),
                        None => url.clone(),
                    },
                    BitcoindAuth::CookieFile(path) => {
                        options.push(format!("cookie={}", path.display()));
                        url.clone()
                    }
                };
                if let Some(height) = birthday_height {
                    options.push(format!("birthday={height}"));
                }
                if options.is_empty() {
                    format!("bitcoind:{authority}")
                } else {
                    format!("bitcoind:{authority}?{}", options.join("&"))
                }
            }
        }
    }

    /// Build the backend. This performs no network I/O, so opening a wallet
    /// works while the endpoint is unreachable.
    pub fn connect(&self) -> anyhow::Result<Arc<dyn ChainBackend>> {
        let backend: Arc<dyn ChainBackend> = match self {
            Self::Esplora { url } => Arc::new(esplora::EsploraBackend::connect(url)?),
            Self::Electrum { url } => Arc::new(electrum::ElectrumBackend::connect(url)?),
            Self::Bitcoind {
                url,
                auth,
                birthday_height,
            } => Arc::new(bitcoind::BitcoindBackend::connect(
                url,
                auth,
                *birthday_height,
            )?),
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
        if let Some(rest) = source.strip_prefix("electrum:") {
            let rest = rest.trim();
            anyhow::ensure!(!rest.is_empty(), "electrum URL must not be empty");
            return Ok(Self::electrum(rest));
        }
        if let Some(rest) = source.strip_prefix("bitcoind:") {
            return parse_bitcoind(rest.trim());
        }
        if source.starts_with("http://") || source.starts_with("https://") {
            // Backwards compatible with the previous `esplora_url` parameter.
            return Ok(Self::esplora(source));
        }
        if source.starts_with("tcp://") || source.starts_with("ssl://") {
            return Ok(Self::electrum(source));
        }
        anyhow::bail!(
            "unrecognised chain source `{source}`: expected `esplora:http://…`, \
             `electrum:tcp://…`, `bitcoind:http://…`, or a bare http(s)/tcp/ssl URL"
        )
    }
}

fn parse_bitcoind(rest: &str) -> anyhow::Result<ChainBackendConfig> {
    let (raw_url, query) = match rest.split_once('?') {
        Some((url, query)) => (url, Some(query)),
        None => (rest, None),
    };
    let (scheme, authority_and_path) = if let Some(after) = raw_url.strip_prefix("http://") {
        ("http://", after)
    } else if let Some(after) = raw_url.strip_prefix("https://") {
        ("https://", after)
    } else {
        anyhow::bail!("bitcoind URL must start with http:// or https:// (got `{rest}`)");
    };

    // `rsplit_once` keeps a host-less path containing `@` intact. A password
    // containing `@` is not supported through the URL; use `?cookie=` for such
    // nodes (documented in ARCHITECTURE.md).
    let (userinfo, host_and_path) = match authority_and_path.rsplit_once('@') {
        Some((userinfo, host_and_path)) => (Some(userinfo), host_and_path),
        None => (None, authority_and_path),
    };
    anyhow::ensure!(
        !host_and_path.trim().is_empty(),
        "bitcoind URL is missing a host: `{rest}`"
    );

    let mut auth = match userinfo {
        Some(userinfo) => {
            let (user, password) = userinfo.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("bitcoind credentials must be `user:password` (got `{userinfo}`)")
            })?;
            BitcoindAuth::UserPass {
                user: user.to_string(),
                password: password.to_string(),
            }
        }
        None => BitcoindAuth::None,
    };
    let mut birthday_height = None;

    for pair in query.unwrap_or_default().split('&') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("bitcoind option `{pair}` must be `key=value`"))?;
        match key {
            "cookie" => {
                anyhow::ensure!(
                    !value.is_empty(),
                    "bitcoind `cookie` option must not be empty"
                );
                auth = BitcoindAuth::CookieFile(PathBuf::from(value));
            }
            "birthday" => {
                let height = value
                    .parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("bitcoind `birthday` must be a block height"))?;
                birthday_height = Some(height);
            }
            other => anyhow::bail!("unknown bitcoind option `{other}`"),
        }
    }

    Ok(ChainBackendConfig::Bitcoind {
        url: format!("{scheme}{host_and_path}"),
        auth,
        birthday_height,
    })
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
    fn electrum_prefix_and_bare_schemes_are_accepted() {
        let config: ChainBackendConfig = "electrum:tcp://127.0.0.1:60401".parse().expect("parse");
        assert_eq!(config.kind(), ChainKind::Electrum);
        assert_eq!(config.canonical(), "electrum:tcp://127.0.0.1:60401");

        let bare: ChainBackendConfig = "ssl://electrum.example:50002".parse().expect("parse");
        assert_eq!(bare.kind(), ChainKind::Electrum);
    }

    #[test]
    fn bitcoind_credentials_are_split_out_of_the_url() {
        let config: ChainBackendConfig = "bitcoind:http://alice:s3cret@127.0.0.1:18443"
            .parse()
            .expect("parse");
        let ChainBackendConfig::Bitcoind { url, auth, .. } = &config else {
            panic!("expected bitcoind");
        };
        assert_eq!(url, "http://127.0.0.1:18443");
        assert_eq!(
            auth,
            &BitcoindAuth::UserPass {
                user: "alice".to_string(),
                password: "s3cret".to_string(),
            }
        );
        assert!(!config.describe().contains("s3cret"));
        assert!(config.canonical().contains("alice:s3cret@127.0.0.1:18443"));
    }

    #[test]
    fn bitcoind_cookie_and_birthday_options_are_parsed() {
        let config: ChainBackendConfig =
            "bitcoind:http://127.0.0.1:8332?cookie=/home/me/.bitcoin/.cookie&birthday=850000"
                .parse()
                .expect("parse");
        let ChainBackendConfig::Bitcoind {
            url,
            auth,
            birthday_height,
        } = &config
        else {
            panic!("expected bitcoind");
        };
        assert_eq!(url, "http://127.0.0.1:8332");
        assert_eq!(
            auth,
            &BitcoindAuth::CookieFile(PathBuf::from("/home/me/.bitcoin/.cookie"))
        );
        assert_eq!(*birthday_height, Some(850_000));
    }

    #[test]
    fn bitcoind_cookie_overrides_url_credentials() {
        let config: ChainBackendConfig =
            "bitcoind:http://alice:pw@127.0.0.1:8332?cookie=/tmp/.cookie"
                .parse()
                .expect("parse");
        let ChainBackendConfig::Bitcoind { auth, .. } = &config else {
            panic!("expected bitcoind");
        };
        assert!(matches!(auth, BitcoindAuth::CookieFile(_)));
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!("ftp://example".parse::<ChainBackendConfig>().is_err());
        assert!("".parse::<ChainBackendConfig>().is_err());
        assert!("esplora:".parse::<ChainBackendConfig>().is_err());
        assert!("electrum:  ".parse::<ChainBackendConfig>().is_err());
        assert!(
            "bitcoind:tcp://127.0.0.1:8332"
                .parse::<ChainBackendConfig>()
                .is_err()
        );
        assert!(
            "bitcoind:http://127.0.0.1:8332?birthday=soon"
                .parse::<ChainBackendConfig>()
                .is_err()
        );
        assert!(
            "bitcoind:http://127.0.0.1:8332?nope=1"
                .parse::<ChainBackendConfig>()
                .is_err()
        );
    }

    #[test]
    fn connect_does_no_network_io() {
        // Constructing a backend against an unreachable endpoint must succeed:
        // Esplora opens HTTP per request, Electrum connects per operation and
        // bitcoind's transport connects per RPC.
        for source in [
            "http://127.0.0.1:1",
            "electrum:tcp://127.0.0.1:1",
            "bitcoind:http://127.0.0.1:1?birthday=0",
        ] {
            let config: ChainBackendConfig = source.parse().expect("parse");
            let backend = config.connect().expect("connect");
            assert_eq!(backend.kind(), config.kind());
            assert!(!backend.describe().is_empty());
        }
    }
}
