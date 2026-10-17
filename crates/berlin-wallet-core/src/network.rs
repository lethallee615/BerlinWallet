//! Client-side chain-id → network derivation.
//!
//! `network_from_chain_id` lives in `fedimint-server-core`
//! (`bitcoin_rpc.rs:35`), which is not a client dependency. The four
//! well-known block-1 hashes are replicated here exactly; unknown hashes map
//! to `Network::Regtest`.

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

/// Derive the Bitcoin network from the block-1 hash.
///
/// Mutinynet and signet both map to `Network::Signet`: the address format is
/// identical, so `Network` alone cannot distinguish them. The chain source URL
/// must be chosen separately.
pub fn network_from_chain_id(chain_id: ChainId) -> Network {
    match chain_id.to_string().as_str() {
        MAINNET_CHAIN_ID => Network::Bitcoin,
        TESTNET_CHAIN_ID => Network::Testnet,
        SIGNET_4_CHAIN_ID | MUTINYNET_CHAIN_ID => Network::Signet,
        _ => Network::Regtest,
    }
}

/// Read the federation-reported chain id (cached in the client DB) and derive
/// the network used for BDK address generation.
pub async fn client_network(client: &Client) -> anyhow::Result<Network> {
    Ok(network_from_chain_id(client.chain_id().await?))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

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
            assert_eq!(network_from_chain_id(chain_id), expected);
        }
    }

    #[test]
    fn unknown_chain_id_falls_back_to_regtest() {
        let chain_id =
            ChainId::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .expect("valid block hash");
        assert_eq!(network_from_chain_id(chain_id), Network::Regtest);
    }
}
