//! Persisted chain-source selection (`<data-dir>/chain.json`).
//!
//! The chain source used to be a per-call Esplora URL, so a restart could not
//! disagree with itself. With more than one backend, silently switching (for
//! example opening an Electrum-backed wallet with the default Esplora URL) would
//! hide a misconfiguration and, on a block-walk backend, lose the scan range.
//! The choice is therefore recorded in the data directory and a mismatch is
//! reported, never taken silently.
//!
//! The file is small, versioned, and written mode 0600. It can contain
//! credentials (a bitcoind `user:password` in the URL) exactly like `mnemonic`,
//! `bdk.sqlite`, and `fedimint.rocksdb` already can; the backup documentation
//! covers all of them together.

use std::path::Path;

use super::ChainBackendConfig;

/// File inside the data directory.
pub const CHAIN_CONFIG_FILE: &str = "chain.json";
const ROW_VERSION: u8 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct ChainConfigEnvelope {
    version: u8,
    /// Canonical, round-trippable chain source (see
    /// [`ChainBackendConfig::canonical`]).
    source: String,
}

fn path_for(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(CHAIN_CONFIG_FILE)
}

/// Read the stored chain source, if any.
pub fn load_chain_config(data_dir: &Path) -> anyhow::Result<Option<ChainBackendConfig>> {
    let path = path_for(data_dir);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::anyhow!("could not read {}: {err}", path.display()));
        }
    };
    let envelope: ChainConfigEnvelope = serde_json::from_slice(&data)
        .map_err(|err| anyhow::anyhow!("{} is not valid JSON: {err}", path.display()))?;
    anyhow::ensure!(
        envelope.version == ROW_VERSION,
        "{} has version {}; this build understands version {ROW_VERSION}",
        path.display(),
        envelope.version
    );
    let config: ChainBackendConfig = envelope.source.parse().map_err(|err| {
        anyhow::anyhow!("{} has an unusable chain source: {err:#}", path.display())
    })?;
    Ok(Some(config))
}

/// Write the chain source. Replaces the file atomically enough for a single
/// process: temp file in the same directory, then rename.
pub fn store_chain_config(data_dir: &Path, config: &ChainBackendConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = path_for(data_dir);
    let envelope = ChainConfigEnvelope {
        version: ROW_VERSION,
        source: config.canonical(),
    };
    let body = serde_json::to_vec_pretty(&envelope)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Resolve and remember the chain source for `data_dir`.
///
/// - `requested` present: use it. If it differs from the stored value, log a
///   warning and overwrite the stored value; a backend change is never silent.
/// - `requested` absent: use the stored value.
/// - neither: return `None` and let the caller decide (the CLI falls back to its
///   default, the FFI asks the caller).
pub fn resolve_chain_config(
    data_dir: &Path,
    requested: Option<&str>,
) -> anyhow::Result<Option<ChainBackendConfig>> {
    let requested = requested
        .map(str::trim)
        .filter(|source| !source.is_empty())
        .map(str::parse::<ChainBackendConfig>)
        .transpose()?;
    let stored = load_chain_config(data_dir)?;

    let config = match (requested, stored) {
        (Some(requested), Some(stored)) => {
            if requested != stored {
                tracing::warn!(
                    stored = %stored.canonical(),
                    requested = %requested.canonical(),
                    "chain source changed for this data directory; using the requested one"
                );
            }
            requested
        }
        (Some(requested), None) => requested,
        (None, Some(stored)) => stored,
        (None, None) => return Ok(None),
    };

    store_chain_config(data_dir, &config)?;
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("berlin-wallet-chain-cfg-{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn esplora_round_trips() {
        let dir = temp_dir();
        let config = ChainBackendConfig::esplora("http://127.0.0.1:3002");
        store_chain_config(&dir, &config).expect("store");
        assert_eq!(load_chain_config(&dir).expect("load"), Some(config));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn electrum_round_trips() {
        let dir = temp_dir();
        let config = ChainBackendConfig::electrum("tcp://127.0.0.1:60401");
        store_chain_config(&dir, &config).expect("store");
        assert_eq!(load_chain_config(&dir).expect("load"), Some(config));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = temp_dir();
        assert_eq!(load_chain_config(&dir).expect("load"), None);
    }

    #[test]
    fn resolve_prefers_the_requested_source_and_remembers_it() {
        let dir = temp_dir();
        let first = resolve_chain_config(&dir, Some("electrum:tcp://127.0.0.1:60401"))
            .expect("resolve")
            .expect("configured");
        assert_eq!(first.kind(), crate::chain::ChainKind::Electrum);

        // No explicit source: the stored one wins.
        let second = resolve_chain_config(&dir, None)
            .expect("resolve")
            .expect("configured");
        assert_eq!(first, second);

        // An explicit change is recorded rather than silently ignored.
        let third = resolve_chain_config(&dir, Some("http://127.0.0.1:3002"))
            .expect("resolve")
            .expect("configured");
        assert_eq!(third.kind(), crate::chain::ChainKind::Esplora);
        assert_eq!(
            load_chain_config(&dir).expect("load"),
            Some(third),
            "the new choice must be persisted"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn resolve_returns_none_without_any_choice() {
        let dir = temp_dir();
        assert_eq!(resolve_chain_config(&dir, None).expect("resolve"), None);
        assert_eq!(
            resolve_chain_config(&dir, Some("  ")).expect("resolve"),
            None
        );
    }

    #[test]
    fn a_corrupt_file_is_reported() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CHAIN_CONFIG_FILE), b"{not json").unwrap();
        let err = load_chain_config(&dir).expect_err("corrupt file");
        assert!(err.to_string().contains("valid JSON"), "{err:#}");
        std::fs::remove_dir_all(dir).ok();
    }
}
