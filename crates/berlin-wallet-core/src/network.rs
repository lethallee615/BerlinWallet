//! Client-side chain-id → network derivation and the recorded network choice.
//!
//! `network_from_chain_id` lives in `fedimint-server-core`
//! (`bitcoin_rpc.rs:35`), which is not a client dependency. The four
//! well-known block-1 hashes are replicated here exactly.
//!
//! The upstream helper maps an unknown chain-id to `Network::Regtest`. That is
//! convenient for custom networks and dangerous everywhere else: address
//! prefixes and BDK descriptors silently change. This module fails closed. The
//! one exception is an **explicit** `regtest` choice recorded in the data
//! directory, which is how devimint and private chains are used.

use std::path::Path;

use bitcoin::Network;
use fedimint_client::Client;
use fedimint_core::ChainId;

// <https://blockstream.info/api/block-height/1>
const MAINNET_CHAIN_ID: &str = "00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048";
// <https://blockstream.info/testnet/api/block-height/1>
const TESTNET_CHAIN_ID: &str = "00000000b873e79784647a6c82962c70d228557d24a747ea4d1b8bbe878e1206";
// <https://mempool.space/signet/api/block-height/1>
const SIGNET_4_CHAIN_ID: &str = "00000086d6b2636cb2a392d45edc4ec544a10024d30141c9adf4bfd9de533b53";
// <https://mutinynet.com/api/block-height/1>
const MUTINYNET_CHAIN_ID: &str = "000002855893a0a9b24eaffc5efc770558a326fee4fc10c9da22fc19cd2954f9";

/// File inside the data directory that records the operator's network choice.
pub const NETWORK_FILE: &str = "network.json";
const NETWORK_ROW_VERSION: u8 = 1;

/// Parse a user-facing network name. `mainnet` and `bitcoin` are aliases.
pub fn parse_network(value: &str) -> anyhow::Result<Network> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mainnet" | "bitcoin" => Ok(Network::Bitcoin),
        "testnet" | "testnet3" => Ok(Network::Testnet),
        "signet" => Ok(Network::Signet),
        "regtest" => Ok(Network::Regtest),
        other => {
            anyhow::bail!("unknown network {other:?}; expected mainnet|testnet|signet|regtest")
        }
    }
}

/// Derive the network from the federation's block-1 hash.
///
/// Mutinynet and signet both map to `Network::Signet`: the address format is
/// identical, so `Network` alone cannot distinguish them. The chain source URL
/// must be chosen separately.
pub fn network_from_chain_id(chain_id: ChainId) -> anyhow::Result<Network> {
    match chain_id.to_string().as_str() {
        MAINNET_CHAIN_ID => Ok(Network::Bitcoin),
        TESTNET_CHAIN_ID => Ok(Network::Testnet),
        SIGNET_4_CHAIN_ID | MUTINYNET_CHAIN_ID => Ok(Network::Signet),
        _ => anyhow::bail!(
            "unrecognised federation chain-id {chain_id}; this wallet fails closed. \
             For a custom or regtest federation, initialize the data directory with \
             `--network regtest` and pass `--network regtest` when inspecting it"
        ),
    }
}

/// Verify the chain-id against the network recorded in the data directory.
///
/// A known chain-id must match exactly. An unknown chain-id is accepted only
/// when the recorded choice is `regtest`, which is the explicit opt-in used for
/// devimint and private networks.
pub fn network_from_recorded(chain_id: ChainId, recorded: Network) -> anyhow::Result<Network> {
    match network_from_chain_id(chain_id) {
        Ok(derived) if derived != recorded => anyhow::bail!(
            "federation chain-id is {derived}, but this data directory is recorded as \
             {recorded}; refusing to open with the wrong network"
        ),
        Ok(derived) => Ok(derived),
        Err(_) if recorded == Network::Regtest => Ok(Network::Regtest),
        Err(err) => Err(err.context(format!(
            "the data directory is recorded as {recorded}; only an explicit regtest \
             choice may use an unrecognised chain-id"
        ))),
    }
}

/// Read the federation-reported chain id and derive the network. Fails closed
/// on an unrecognised chain-id.
pub async fn client_network(client: &Client) -> anyhow::Result<Network> {
    network_from_chain_id(client.chain_id().await?)
}

/// Read the chain id and verify it against the recorded network choice.
pub async fn verified_client_network(
    client: &Client,
    recorded: Network,
) -> anyhow::Result<Network> {
    network_from_recorded(client.chain_id().await?, recorded)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NetworkEnvelope {
    version: u8,
    network: String,
}

fn path_for(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(NETWORK_FILE)
}

/// Read the recorded network, if any.
pub fn load_network(data_dir: &Path) -> anyhow::Result<Option<Network>> {
    let path = path_for(data_dir);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow::anyhow!("could not read {}: {err}", path.display()));
        }
    };
    let envelope: NetworkEnvelope = serde_json::from_slice(&data)
        .map_err(|err| anyhow::anyhow!("{} is not valid JSON: {err}", path.display()))?;
    anyhow::ensure!(
        envelope.version == NETWORK_ROW_VERSION,
        "{} has version {}; this build understands version {NETWORK_ROW_VERSION}",
        path.display(),
        envelope.version
    );
    let network = parse_network(&envelope.network)
        .map_err(|err| anyhow::anyhow!("{} has an unusable network: {err:#}", path.display()))?;
    Ok(Some(network))
}

/// Write the network choice. Atomic enough for one process: temp file in the
/// same directory, mode 0600, then rename.
pub fn store_network(data_dir: &Path, network: Network) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = path_for(data_dir);
    let envelope = NetworkEnvelope {
        version: NETWORK_ROW_VERSION,
        network: network.to_string(),
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

/// Record the operator's choice, or verify that it agrees with an existing
/// record. Refuses to silently change the network of an initialized directory.
pub fn init_network(data_dir: &Path, requested: Network) -> anyhow::Result<Network> {
    if let Some(stored) = load_network(data_dir)? {
        anyhow::ensure!(
            stored == requested,
            "data directory is already initialized for {stored}; refusing to change it to \
             {requested}. Use a fresh data directory for a different network"
        );
        return Ok(stored);
    }
    store_network(data_dir, requested)?;
    Ok(requested)
}

/// Read the recorded network or explain how to create it.
pub fn require_recorded_network(data_dir: &Path) -> anyhow::Result<Network> {
    load_network(data_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no network is recorded in {}; initialize the data directory with \
             `--network <mainnet|testnet|signet|regtest>` first",
            path_for(data_dir).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::str::FromStr;

    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("berlin-wallet-network-{}", uuid::Uuid::now_v7()))
    }

    fn unknown_chain_id() -> ChainId {
        ChainId::from_str("0000000000000000000000000000000000000000000000000000000000000001")
            .expect("valid block hash")
    }

    #[test]
    fn known_chain_ids_map_to_networks() {
        let cases = [
            (MAINNET_CHAIN_ID, Network::Bitcoin),
            (TESTNET_CHAIN_ID, Network::Testnet),
            (SIGNET_4_CHAIN_ID, Network::Signet),
            (MUTINYNET_CHAIN_ID, Network::Signet),
        ];
        for (chain_id, expected) in cases {
            let chain_id = ChainId::from_str(chain_id).expect("valid block hash");
            assert_eq!(network_from_chain_id(chain_id).expect("known"), expected);
        }
    }

    #[test]
    fn unknown_chain_id_fails_closed() {
        let err = network_from_chain_id(unknown_chain_id()).expect_err("unknown must fail");
        assert!(err.to_string().contains("unrecognised"), "{err:#}");
        assert!(err.to_string().contains("--network regtest"), "{err:#}");
    }

    #[test]
    fn a_known_chain_id_must_match_the_recorded_network() {
        let chain_id = ChainId::from_str(MAINNET_CHAIN_ID).expect("valid");
        assert_eq!(
            network_from_recorded(chain_id, Network::Bitcoin).expect("match"),
            Network::Bitcoin
        );
        let err = network_from_recorded(chain_id, Network::Testnet).expect_err("mismatch");
        assert!(err.to_string().contains("refusing"), "{err:#}");
    }

    #[test]
    fn an_unknown_chain_id_requires_an_explicit_regtest_choice() {
        assert_eq!(
            network_from_recorded(unknown_chain_id(), Network::Regtest).expect("explicit opt-in"),
            Network::Regtest
        );
        let err =
            network_from_recorded(unknown_chain_id(), Network::Signet).expect_err("not regtest");
        assert!(err.to_string().contains("explicit regtest"), "{err:#}");
    }

    #[test]
    fn the_network_file_round_trips() {
        let dir = temp_dir();
        assert_eq!(load_network(&dir).expect("load"), None);
        store_network(&dir, Network::Signet).expect("store");
        assert_eq!(load_network(&dir).expect("load"), Some(Network::Signet));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn init_refuses_to_change_an_existing_network() {
        let dir = temp_dir();
        assert_eq!(
            init_network(&dir, Network::Regtest).expect("first"),
            Network::Regtest
        );
        assert_eq!(
            init_network(&dir, Network::Regtest).expect("same"),
            Network::Regtest
        );
        let err = init_network(&dir, Network::Signet).expect_err("change");
        assert!(err.to_string().contains("refusing"), "{err:#}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_missing_record_has_a_clear_error() {
        let dir = temp_dir();
        let err = require_recorded_network(&dir).expect_err("missing");
        assert!(err.to_string().contains("--network"), "{err:#}");
    }

    #[test]
    fn a_corrupt_file_is_reported() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(NETWORK_FILE), b"{not json").unwrap();
        let err = load_network(&dir).expect_err("corrupt");
        assert!(err.to_string().contains("valid JSON"), "{err:#}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn network_names_are_user_friendly() {
        assert_eq!(parse_network("mainnet").unwrap(), Network::Bitcoin);
        assert_eq!(parse_network("Bitcoin").unwrap(), Network::Bitcoin);
        assert_eq!(parse_network("testnet").unwrap(), Network::Testnet);
        assert_eq!(parse_network("signet").unwrap(), Network::Signet);
        assert_eq!(parse_network("regtest").unwrap(), Network::Regtest);
        assert!(parse_network("mainnet2").is_err());
    }
}
