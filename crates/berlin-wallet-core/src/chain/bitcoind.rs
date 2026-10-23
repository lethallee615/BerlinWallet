//! bitcoind JSON-RPC backend (`bdk_bitcoind_rpc 0.22.0` → `bitcoincore-rpc 0.19.0`).
//!
//! This backend is **not** a script-pubkey query client. `bdk_bitcoind_rpc`
//! exposes a block `Emitter` (`src/lib.rs:81,202`): it walks blocks forward from
//! a starting height and the wallet applies them one at a time with
//! `apply_block_connected_to` (`bdk_wallet/src/wallet/mod.rs:2702`). That is why
//! `ChainBackend::sync` takes `&mut Wallet` instead of returning a
//! `SyncResponse`: there is no response type to return here.
//!
//! Two consequences are deliberate and documented in `ARCHITECTURE.md`:
//!
//! - A full scan needs a **birthday height**. There is no gap-limit discovery:
//!   a block walk can only see payments to scripts the wallet tracks, so the
//!   scan reveals a bounded range derived from `stop_gap` and walks blocks from
//!   the birthday. Scanning from genesis is refused outside regtest.
//! - The RPC transport is blocking, so every call runs in `spawn_blocking`.
//!   `bitcoincore_rpc::Client::new` cannot set a socket timeout, so the client is
//!   built through `jsonrpc::simple_http::Builder` and
//!   `bitcoincore_rpc::Client::from_jsonrpc`
//!   (`bitcoincore-rpc/src/client.rs:1288,1297`).
//!
//! BIP158 compact filters (`bdk_bitcoind_rpc::bip158::FilterIter`) are the
//! cheaper alternative for a large first scan, but they need a pre-materialized
//! script list and a node with the block filter index. They are not used here;
//! `getcfilters` and the filter-index requirement must be verified first.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bdk_bitcoind_rpc::bitcoincore_rpc::{self, RpcApi, jsonrpc};
use bdk_bitcoind_rpc::{Emitter, NO_EXPECTED_MEMPOOL_TXS};
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{BlockHash, Network, Transaction};

use super::{
    BitcoindAuth, ChainBackend, ChainError, ChainKind, ChainTip, DEFAULT_STOP_GAP, FullScanOptions,
};

/// Socket timeout for one JSON-RPC request.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall-clock bound for one batch of blocks.
const BATCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Blocks applied per batch. Bounds memory: a mainnet block is ~1.5 MB, so this
/// is roughly 300 MB in the worst case.
const BLOCKS_PER_BATCH: usize = 200;
/// How far past `stop_gap` a bounded full scan reveals scripts. This is the
/// discovery limit of this backend.
pub const SPK_LOOKAHEAD_FACTOR: u32 = 5;

/// `-8` (`RPC_INVALID_PARAMETER`): Core's answer for a height above the tip.
const RPC_INVALID_PARAMETER: i32 = -8;
/// `-27` (`RPC_TRANSACTION_ALREADY_IN_CHAIN`).
const RPC_TRANSACTION_ALREADY_IN_CHAIN: i32 = -27;

/// bitcoind RPC client wrapper.
pub struct BitcoindBackend {
    client: Arc<bitcoincore_rpc::Client>,
    url: String,
    birthday_height: Option<u32>,
}

impl BitcoindBackend {
    pub fn connect(
        url: &str,
        auth: &BitcoindAuth,
        birthday_height: Option<u32>,
    ) -> anyhow::Result<Self> {
        let url = url.trim();
        anyhow::ensure!(
            url.starts_with("http://") || url.starts_with("https://"),
            "bitcoind URL must start with http:// or https:// (got `{url}`)"
        );

        let builder = jsonrpc::simple_http::Builder::new()
            .url(url)
            .map_err(|err| anyhow::anyhow!("invalid bitcoind URL `{url}`: {err}"))?
            .timeout(RPC_TIMEOUT);
        let builder = match auth {
            BitcoindAuth::None => builder,
            BitcoindAuth::UserPass { user, password } => {
                builder.auth(user.clone(), Some(password.clone()))
            }
            BitcoindAuth::CookieFile(path) => {
                let path = path.to_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "bitcoind cookie path is not valid UTF-8: {}",
                        path.display()
                    )
                })?;
                builder.cookie_auth(path)
            }
        };
        let rpc =
            bitcoincore_rpc::Client::from_jsonrpc(jsonrpc::Client::with_transport(builder.build()));

        Ok(Self {
            client: Arc::new(rpc),
            url: url.to_string(),
            birthday_height,
        })
    }

    /// Apply every block from `start_height` to the tip, in bounded batches.
    async fn walk_blocks(&self, wallet: &mut Wallet, start_height: u32) -> Result<(), ChainError> {
        loop {
            let checkpoint = wallet.latest_checkpoint();
            let start = start_height.max(checkpoint.height());
            let client = self.client.clone();
            let batch = run_rpc(Some(BATCH_TIMEOUT), move || {
                let mut emitter = Emitter::new(client, checkpoint, start, NO_EXPECTED_MEMPOOL_TXS);
                let mut blocks = Vec::with_capacity(BLOCKS_PER_BATCH);
                while blocks.len() < BLOCKS_PER_BATCH {
                    match emitter.next_block()? {
                        Some(event) => blocks.push(event),
                        None => break,
                    }
                }
                Ok(blocks)
            })
            .await?;

            let caught_up = batch.len() < BLOCKS_PER_BATCH;
            for event in batch {
                let height = event.block_height();
                let connected_to = event.connected_to();
                wallet
                    .apply_block_connected_to(&event.block, height, connected_to)
                    .map_err(|err| {
                        ChainError::permanent(ChainKind::Bitcoind, anyhow::anyhow!(err))
                    })?;
            }
            if caught_up {
                return Ok(());
            }
        }
    }

    /// Report mempool transaction updates and evictions.
    ///
    /// `Emitter::mempool` only reports evictions once its checkpoint is at the
    /// RPC tip, which it is after [`Self::walk_blocks`] returns.
    async fn sync_mempool(&self, wallet: &mut Wallet) -> Result<(), ChainError> {
        let checkpoint = wallet.latest_checkpoint();
        let expected = unconfirmed_txs(wallet);
        let client = self.client.clone();
        let start = checkpoint.height();
        let event = run_rpc(Some(RPC_TIMEOUT), move || {
            let mut emitter = Emitter::new(client, checkpoint, start, expected);
            emitter.mempool()
        })
        .await?;

        if !event.evicted.is_empty() {
            tracing::info!(
                evicted = event.evicted.len(),
                "bitcoind reported evicted mempool transactions"
            );
        }
        wallet.apply_unconfirmed_txs(event.update);
        wallet.apply_evicted_txs(event.evicted);
        Ok(())
    }
}

/// Transactions the wallet currently considers unconfirmed. Passing them to the
/// emitter is what makes eviction detection work for a transaction this wallet
/// created.
fn unconfirmed_txs(wallet: &Wallet) -> Vec<Arc<Transaction>> {
    wallet
        .transactions()
        .filter(|tx| tx.chain_position.is_unconfirmed())
        .map(|tx| Arc::clone(&tx.tx_node.tx))
        .collect()
}

fn last_active(wallet: &Wallet) -> Vec<(KeychainKind, u32)> {
    let index = wallet.spk_index();
    [KeychainKind::External, KeychainKind::Internal]
        .into_iter()
        .filter_map(|keychain| {
            index
                .last_used_index(keychain)
                .map(|index| (keychain, index))
        })
        .collect()
}

/// The script range a bounded block-walk scan covers.
fn scan_target(last_revealed: Option<u32>, stop_gap: usize) -> u32 {
    let base = last_revealed
        .map(|index| index.saturating_add(1))
        .unwrap_or(0)
        .max(u32::try_from(stop_gap).unwrap_or(u32::MAX).max(1));
    base.saturating_mul(SPK_LOOKAHEAD_FACTOR).saturating_sub(1)
}

fn rpc_code(err: &bitcoincore_rpc::Error) -> Option<i32> {
    match err {
        bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(rpc)) => Some(rpc.code),
        _ => None,
    }
}

fn rpc_message(err: &bitcoincore_rpc::Error) -> Option<&str> {
    match err {
        bitcoincore_rpc::Error::ReturnedError(message) => Some(message),
        bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(rpc)) => Some(&rpc.message),
        _ => None,
    }
}

/// A re-broadcast of a transaction the node already has is success for our
/// purposes: `fund_deposit` can re-run after a crash and rebuild the same
/// payment.
fn is_already_known(err: &bitcoincore_rpc::Error) -> bool {
    if rpc_code(err) == Some(RPC_TRANSACTION_ALREADY_IN_CHAIN) {
        return true;
    }
    rpc_message(err)
        .map(|message| message.to_ascii_lowercase().contains("already"))
        .unwrap_or(false)
}

/// Transport failures can fix themselves; RPC rejections and local decode
/// failures cannot.
fn classify(err: bitcoincore_rpc::Error) -> ChainError {
    let transient = matches!(&err, bitcoincore_rpc::Error::Io(_))
        || matches!(
            &err,
            bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Transport(_))
        );
    if transient {
        ChainError::transient(ChainKind::Bitcoind, anyhow::anyhow!(err))
    } else {
        ChainError::permanent(ChainKind::Bitcoind, anyhow::anyhow!(err))
    }
}

/// Await a blocking RPC task, returning the raw RPC result so callers can
/// inspect the error code before classifying it.
async fn join_rpc<T>(
    timeout: Option<Duration>,
    handle: tokio::task::JoinHandle<Result<T, bitcoincore_rpc::Error>>,
) -> Result<Result<T, bitcoincore_rpc::Error>, ChainError> {
    let joined = match timeout {
        Some(limit) => match tokio::time::timeout(limit, handle).await {
            Ok(joined) => joined,
            // The blocking task keeps running until its socket timeout fires.
            Err(_) => {
                return Err(ChainError::transient(
                    ChainKind::Bitcoind,
                    anyhow::anyhow!("bitcoind request timed out"),
                ));
            }
        },
        None => handle.await,
    };
    joined.map_err(|join_err| {
        ChainError::permanent(
            ChainKind::Bitcoind,
            anyhow::anyhow!("bitcoind blocking task panicked: {join_err}"),
        )
    })
}

async fn run_rpc<T, F>(timeout: Option<Duration>, work: F) -> Result<T, ChainError>
where
    F: FnOnce() -> Result<T, bitcoincore_rpc::Error> + Send + 'static,
    T: Send + 'static,
{
    match join_rpc(timeout, tokio::task::spawn_blocking(work)).await? {
        Ok(value) => Ok(value),
        Err(err) => Err(classify(err)),
    }
}

#[async_trait]
impl ChainBackend for BitcoindBackend {
    fn kind(&self) -> ChainKind {
        ChainKind::Bitcoind
    }

    fn describe(&self) -> String {
        match self.birthday_height {
            Some(height) => format!("bitcoind: {} (birthday {height})", self.url),
            None => format!("bitcoind: {}", self.url),
        }
    }

    async fn tip(&self) -> Result<ChainTip, ChainError> {
        let client = self.client.clone();
        run_rpc(Some(RPC_TIMEOUT), move || {
            let height = u32::try_from(client.get_block_count()?)
                .map_err(|_| bitcoincore_rpc::Error::ReturnedError("tip height overflow".into()))?;
            let hash = client.get_block_hash(u64::from(height))?;
            Ok(ChainTip { height, hash })
        })
        .await
    }

    async fn block_hash_at(&self, height: u32) -> Result<Option<BlockHash>, ChainError> {
        let client = self.client.clone();
        match join_rpc(
            Some(RPC_TIMEOUT),
            tokio::task::spawn_blocking(move || client.get_block_hash(u64::from(height))),
        )
        .await?
        {
            Ok(hash) => Ok(Some(hash)),
            // A height above the tip is a definitive "not on the best chain".
            Err(err) if rpc_code(&err) == Some(RPC_INVALID_PARAMETER) => Ok(None),
            Err(err) => Err(classify(err)),
        }
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<(), ChainError> {
        let client = self.client.clone();
        let tx = tx.clone();
        match join_rpc(
            Some(RPC_TIMEOUT),
            tokio::task::spawn_blocking(move || client.send_raw_transaction(&tx)),
        )
        .await?
        {
            Ok(txid) => {
                tracing::debug!(%txid, "bitcoind accepted the transaction");
                Ok(())
            }
            Err(err) if is_already_known(&err) => {
                tracing::info!(
                    error = %err,
                    "bitcoind reports the transaction is already known; treating broadcast as success"
                );
                Ok(())
            }
            Err(err) => Err(classify(err)),
        }
    }

    async fn sync(&self, wallet: &mut Wallet) -> Result<(), ChainError> {
        self.walk_blocks(wallet, 0).await?;
        self.sync_mempool(wallet).await
    }

    async fn full_scan(
        &self,
        wallet: &mut Wallet,
        options: FullScanOptions,
    ) -> Result<Vec<(KeychainKind, u32)>, ChainError> {
        let birthday = options
            .birthday_height
            .or(self.birthday_height)
            .ok_or_else(|| {
                ChainError::unsupported(
                    ChainKind::Bitcoind,
                    "a full scan without a birthday height (add `?birthday=<height>` to the chain source)",
                )
            })?;
        if birthday == 0 && wallet.network() != Network::Regtest {
            return Err(ChainError::unsupported(
                ChainKind::Bitcoind,
                "scanning from genesis on a non-regtest network (add `?birthday=<height>` to the chain source)",
            ));
        }

        // A block walk can only discover payments to scripts the wallet tracks,
        // so reveal a bounded range first. This is not gap-limit discovery.
        let request = wallet.start_full_scan().build();
        for keychain in [KeychainKind::External, KeychainKind::Internal] {
            let target = scan_target(request.last_revealed(&keychain), options.stop_gap);
            for _ in wallet.reveal_addresses_to(keychain, target) {}
        }

        self.walk_blocks(wallet, birthday).await?;
        self.sync_mempool(wallet).await?;
        Ok(last_active(wallet))
    }

    fn full_scan_options(&self) -> FullScanOptions {
        FullScanOptions {
            stop_gap: DEFAULT_STOP_GAP,
            birthday_height: self.birthday_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_target_has_a_floor_and_a_bound() {
        assert_eq!(scan_target(None, 20), 99);
        assert_eq!(scan_target(Some(9), 20), 99);
        assert_eq!(scan_target(Some(40), 20), 204);
    }

    #[test]
    fn already_known_codes_are_recognised() {
        let err = bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(jsonrpc::error::RpcError {
            code: RPC_TRANSACTION_ALREADY_IN_CHAIN,
            message: "transaction already in block chain".to_string(),
            data: None,
        }));
        assert!(is_already_known(&err));

        let err = bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(jsonrpc::error::RpcError {
            code: -26,
            message: "txn-already-known".to_string(),
            data: None,
        }));
        assert!(is_already_known(&err));

        let err = bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(jsonrpc::error::RpcError {
            code: -26,
            message: "min relay fee not met".to_string(),
            data: None,
        }));
        assert!(!is_already_known(&err));
    }

    #[test]
    fn rpc_rejections_are_permanent_and_io_is_transient() {
        let io = bitcoincore_rpc::Error::Io(std::io::Error::other("connection refused"));
        assert!(classify(io).is_transient());

        let rpc = bitcoincore_rpc::Error::JsonRpc(jsonrpc::Error::Rpc(jsonrpc::error::RpcError {
            code: RPC_INVALID_PARAMETER,
            message: "Block height out of range".to_string(),
            data: None,
        }));
        assert!(!classify(rpc).is_transient());
    }

    #[test]
    fn connect_requires_an_http_url() {
        assert!(
            BitcoindBackend::connect("http://127.0.0.1:8332", &BitcoindAuth::None, None).is_ok()
        );
        assert!(
            BitcoindBackend::connect("tcp://127.0.0.1:8332", &BitcoindAuth::None, None).is_err()
        );
    }

    #[tokio::test]
    async fn an_unreachable_node_is_a_transient_error() {
        // Port 1 on loopback is never listening: the real transport and the
        // classifier the retry loop depends on are both exercised.
        let backend =
            BitcoindBackend::connect("http://127.0.0.1:1", &BitcoindAuth::None, None).unwrap();
        let err = backend.tip().await.expect_err("unreachable node");
        assert!(err.is_transient(), "unexpected: {err:#}");
    }

    fn test_wallet(network: Network) -> Wallet {
        let (external, internal) =
            crate::bdk::descriptors_from_seed(&[0x33u8; 64], network).expect("descriptors");
        Wallet::create(external, internal)
            .network(network)
            .create_wallet_no_persist()
            .expect("wallet")
    }

    #[tokio::test]
    async fn a_full_scan_without_a_birthday_is_refused() {
        let backend =
            BitcoindBackend::connect("http://127.0.0.1:1", &BitcoindAuth::None, None).unwrap();
        let mut wallet = test_wallet(Network::Regtest);
        let err = backend
            .full_scan(&mut wallet, FullScanOptions::default())
            .await
            .expect_err("no birthday");
        assert!(
            matches!(err, ChainError::Unsupported { .. }),
            "unexpected: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_genesis_scan_is_refused_outside_regtest() {
        let backend =
            BitcoindBackend::connect("http://127.0.0.1:1", &BitcoindAuth::None, Some(0)).unwrap();
        let mut wallet = test_wallet(Network::Bitcoin);
        let err = backend
            .full_scan(&mut wallet, backend.full_scan_options())
            .await
            .expect_err("mainnet genesis scan");
        assert!(
            matches!(err, ChainError::Unsupported { .. }),
            "unexpected: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_regtest_genesis_scan_reaches_the_rpc_layer() {
        // birthday = 0 is allowed on regtest: the guard must not fire, and the
        // failure comes from the unreachable node instead.
        let backend =
            BitcoindBackend::connect("http://127.0.0.1:1", &BitcoindAuth::None, Some(0)).unwrap();
        let mut wallet = test_wallet(Network::Regtest);
        let err = backend
            .full_scan(&mut wallet, backend.full_scan_options())
            .await
            .expect_err("unreachable node");
        assert!(
            !matches!(err, ChainError::Unsupported { .. }),
            "regtest must allow birthday 0, got {err:#}"
        );
    }

    #[test]
    fn full_scan_options_carry_the_birthday() {
        let backend =
            BitcoindBackend::connect("http://127.0.0.1:8332", &BitcoindAuth::None, Some(850_000))
                .expect("connect");
        assert_eq!(
            backend.full_scan_options(),
            FullScanOptions {
                stop_gap: DEFAULT_STOP_GAP,
                birthday_height: Some(850_000),
            }
        );
        assert!(backend.describe().contains("850000"));
    }
}
