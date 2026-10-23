use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use bdk_wallet::KeychainKind;
use bdk_wallet::descriptor::IntoWalletDescriptor;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::signer::{SignOptions, SignersContainer};
use bdk_wallet::{PersistedWallet, Wallet};
use bitcoin::Network;
use bitcoin::bip32::Xpriv;

use crate::chain::{ChainBackend, ChainBackendConfig, ChainError};

/// SQLite file used by the BDK wallet. The CLI places it under its data dir.
pub const BDK_DB_FILE: &str = "bdk.sqlite";

const SYNC_MAX_ATTEMPTS: u32 = 3;
const SYNC_BACKOFF_BASE: Duration = Duration::from_millis(250);
/// Cap the retry backoff so a long outage cannot hold a sync task for minutes.
const SYNC_BACKOFF_MAX: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReorgDecision {
    AncestorOk,
    Orphaned,
}

/// Decide whether the wallet's stored checkpoint is still on the remote chain.
///
/// `remote_at_local` is the remote hash at the local height when the remote tip
/// is at least that high; when the remote tip is lower, the stored tip cannot be
/// an ancestor and the wallet must rebuild/rescan.
fn reorg_decision(
    local_height: u32,
    local_hash: &bitcoin::BlockHash,
    remote_height: u32,
    remote_at_local: Option<bitcoin::BlockHash>,
) -> ReorgDecision {
    if remote_height < local_height {
        return ReorgDecision::Orphaned;
    }
    match remote_at_local {
        Some(remote_hash) if remote_hash == *local_hash => ReorgDecision::AncestorOk,
        _ => ReorgDecision::Orphaned,
    }
}

/// Derive BIP84 external/change descriptors from the BIP39 seed.
///
/// The same BIP39 seed is used for the fedimint root secret, but fedimint
/// applies its own salted `PlainRootSecretStrategy` derivation while BDK uses
/// the standard BIP32/BIP84 hierarchy. No raw key material is shared.
pub fn descriptors_from_seed(
    seed: &[u8; 64],
    network: Network,
) -> anyhow::Result<(String, String)> {
    let coin_type = if network == Network::Bitcoin { 0 } else { 1 };
    let master = Xpriv::new_master(network, seed)?;
    Ok((
        format!("wpkh({master}/84'/{coin_type}'/0'/0/*)"),
        format!("wpkh({master}/84'/{coin_type}'/0'/1/*)"),
    ))
}

fn signers_from_descriptor(wallet: &Wallet, descriptor: &str) -> anyhow::Result<SignersContainer> {
    let (descriptor, keymap) =
        descriptor.into_wallet_descriptor(wallet.secp_ctx(), wallet.network().into())?;
    Ok(SignersContainer::build(
        keymap,
        &descriptor,
        wallet.secp_ctx(),
    ))
}

/// BDK wallet plus its SQLite persister, software signers, and chain backend.
///
/// The wrapper exists because `PersistedWallet<Connection>` needs the
/// `Connection` alongside it for `persist`, and the caller should not have to
/// duplicate descriptor/signer construction.
///
/// Persistence is owned here, never by the chain backend: the backend only
/// stages changes on the wallet, and [`BdkWallet`] writes them in one place.
pub struct BdkWallet {
    wallet: Option<PersistedWallet<Connection>>,
    persister: Option<Connection>,
    external_signers: SignersContainer,
    internal_signers: SignersContainer,
    chain: Arc<dyn ChainBackend>,
    path: std::path::PathBuf,
    seed: [u8; 64],
    network: Network,
}

impl BdkWallet {
    /// Open `path` or create it with BIP84 descriptors derived from `seed`.
    ///
    /// The chain backend is built here but performs no network I/O, so this
    /// works while the endpoint is unreachable.
    pub fn open_or_create(
        path: &Path,
        seed: &[u8; 64],
        network: Network,
        chain: ChainBackendConfig,
    ) -> anyhow::Result<Self> {
        Self::open_with_backend(path, seed, network, chain.connect()?)
    }

    fn open_with_backend(
        path: &Path,
        seed: &[u8; 64],
        network: Network,
        chain: Arc<dyn ChainBackend>,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let (external_descriptor, internal_descriptor) = descriptors_from_seed(seed, network)?;
        let mut persister = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        let load_params = Wallet::load()
            .descriptor(KeychainKind::External, Some(external_descriptor.clone()))
            .descriptor(KeychainKind::Internal, Some(internal_descriptor.clone()))
            .check_network(network);

        let wallet = match PersistedWallet::load(&mut persister, load_params)? {
            Some(wallet) => wallet,
            None => Wallet::create(external_descriptor.clone(), internal_descriptor.clone())
                .network(network)
                .create_wallet(&mut persister)?,
        };

        let external_signers = signers_from_descriptor(&wallet, &external_descriptor)?;
        let internal_signers = signers_from_descriptor(&wallet, &internal_descriptor)?;

        Ok(Self {
            wallet: Some(wallet),
            persister: Some(persister),
            external_signers,
            internal_signers,
            chain,
            path: path.to_path_buf(),
            seed: *seed,
            network,
        })
    }

    fn wallet(&self) -> &PersistedWallet<Connection> {
        self.wallet.as_ref().expect("BDK wallet is present")
    }

    fn wallet_mut(&mut self) -> &mut PersistedWallet<Connection> {
        self.wallet.as_mut().expect("BDK wallet is present")
    }

    pub fn network(&self) -> Network {
        self.wallet().network()
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.wallet().balance()
    }

    /// The chain backend in use.
    pub fn chain_kind(&self) -> crate::chain::ChainKind {
        self.chain.kind()
    }

    /// Backend identity for logs and UI (no credentials).
    pub fn chain_description(&self) -> String {
        self.chain.describe()
    }

    /// True if `address` belongs to this BDK wallet. Only in that case could a
    /// local script scan observe a withdrawal output; external destinations are
    /// reconciled from the federation's `Success(txid)` instead.
    pub fn is_our_address(
        &self,
        address: &bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    ) -> bool {
        self.wallet()
            .is_mine(address.clone().assume_checked().script_pubkey())
    }

    /// Reveal and persist the next external address.
    pub fn reveal_destination(&mut self) -> anyhow::Result<bitcoin::Address> {
        let address = self
            .wallet_mut()
            .reveal_next_address(KeychainKind::External)
            .address;
        self.persist()?;
        Ok(address)
    }

    /// Build, sign, finalize, broadcast, and record an unconfirmed payment.
    pub async fn build_and_finalize(
        &mut self,
        destination: &bitcoin::Address,
        amount: bitcoin::Amount,
    ) -> anyhow::Result<bitcoin::Transaction> {
        let mut builder = self.wallet_mut().build_tx();
        builder.add_recipient(destination.script_pubkey(), amount);
        let mut psbt = builder.finish()?;

        let external_signers = self.external_signers.clone();
        let internal_signers = self.internal_signers.clone();
        let finalized = self.wallet_mut().sign_with_signers(
            &mut psbt,
            &[&external_signers, &internal_signers],
            SignOptions::default(),
        )?;
        anyhow::ensure!(finalized, "BDK could not finalize the PSBT");

        let tx = psbt.extract_tx()?;
        self.chain
            .broadcast(&tx)
            .await
            .map_err(ChainError::into_anyhow)?;
        let last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.wallet_mut()
            .apply_unconfirmed_txs([(tx.clone(), last_seen)]);
        self.persist()?;
        Ok(tx)
    }

    /// Full script-pubkey scan for a restored wallet.
    ///
    /// The backend chooses its own scan strategy: script-pubkey backends start at
    /// derivation index 0 and stop after `stop_gap` unused scripts; a block-walk
    /// backend needs a birthday height instead.
    pub async fn full_scan(&mut self) -> anyhow::Result<Vec<(KeychainKind, u32)>> {
        self.full_scan_inner()
            .await
            .map_err(ChainError::into_anyhow)
    }

    /// [`Self::full_scan`] that keeps the backend's error classification, so a
    /// transient failure during a reorg-triggered rebuild is still retried by
    /// [`Self::sync`].
    async fn full_scan_inner(&mut self) -> Result<Vec<(KeychainKind, u32)>, ChainError> {
        let chain = self.chain.clone();
        let options = chain.full_scan_options();
        let last_active = {
            let wallet: &mut Wallet = self.wallet_mut();
            chain.full_scan(wallet, options).await?
        };
        self.persist()
            .map_err(|err| ChainError::permanent(chain.kind(), err))?;
        Ok(last_active)
    }

    /// Reorg-safe partial sync.
    ///
    /// `bdk_esplora` builds its chain update from the wallet's local tip; a
    /// lower or conflicting remote tip can be a no-op that leaves anchors to
    /// orphaned blocks (`docs/API-VERIFICATION.md` §34). We verify ancestry
    /// against the backend first and, when the checkpoint is orphaned, rebuild
    /// the wallet DB and full-scan instead of trusting the incremental update.
    /// The check is backend-independent.
    pub async fn sync(&mut self) -> anyhow::Result<()> {
        let mut backoff = SYNC_BACKOFF_BASE;
        let mut last_error = None;

        for attempt in 1..=SYNC_MAX_ATTEMPTS {
            match self.sync_attempt().await {
                Ok(()) => return Ok(()),
                Err(err) if err.is_transient() => {
                    last_error = Some(err);
                    if attempt < SYNC_MAX_ATTEMPTS {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(SYNC_BACKOFF_MAX);
                    }
                }
                Err(err) => return Err(err.into_anyhow()),
            }
        }

        Err(last_error
            .expect("a transient error was recorded")
            .into_anyhow())
    }

    async fn sync_attempt(&mut self) -> Result<(), ChainError> {
        let chain = self.chain.clone();
        let local_tip = self.wallet().latest_checkpoint();
        let local_height = local_tip.height();
        let remote = chain.tip().await?;
        let remote_at_local = if remote.height >= local_height {
            chain.block_hash_at(local_height).await?
        } else {
            None
        };

        if reorg_decision(
            local_height,
            &local_tip.hash(),
            remote.height,
            remote_at_local,
        ) == ReorgDecision::Orphaned
        {
            tracing::info!(
                backend = %chain.kind(),
                local_height,
                remote_height = remote.height,
                remote_tip = %remote.hash,
                "BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan"
            );
            // The incremental update cannot lower the local tip (merge_chains
            // returns an empty changeset when the update tip is lower and all
            // overlapping hashes agree). Rebuild from scratch and full-scan.
            self.reset_and_full_scan().await?;
            return Ok(());
        }

        chain.sync(self.wallet_mut()).await?;
        self.persist()
            .map_err(|err| ChainError::permanent(chain.kind(), err))?;
        Ok(())
    }

    /// Rebuild the BDK wallet from the same seed/descriptors and full-scan.
    ///
    /// Used only when the stored checkpoint is orphaned and the incremental
    /// update cannot force the tip down. The old DB is replaced only after the
    /// fresh wallet has successfully full-scanned.
    async fn reset_and_full_scan(&mut self) -> Result<(), ChainError> {
        let temp_path = self.path.with_file_name(format!(
            "{}.reorg-{}",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("bdk.sqlite"),
            uuid::Uuid::now_v7()
        ));
        let mut fresh =
            Self::open_with_backend(&temp_path, &self.seed, self.network, self.chain.clone())
                .map_err(|err| ChainError::permanent(self.chain.kind(), err))?;
        if let Err(err) = fresh.full_scan_inner().await {
            remove_sqlite_files(&temp_path);
            return Err(err);
        }

        self.swap_in_scanned_db(fresh)
            .map_err(|err| ChainError::permanent(self.chain.kind(), err))
    }

    /// Replace this wallet's DB with an already-scanned temp DB.
    ///
    /// The SQLite connection follows the inode, so the temp connection must be
    /// closed before the rename and a new connection opened against the final
    /// path. Both DBs are checkpointed first; the final rename is atomic. On a
    /// crash before the rename the old DB is intact; after the rename the new
    /// (fully scanned) DB is valid.
    fn swap_in_scanned_db(&mut self, temp: BdkWallet) -> anyhow::Result<()> {
        let previous_external = self.wallet().next_derivation_index(KeychainKind::External);
        let previous_internal = self.wallet().next_derivation_index(KeychainKind::Internal);
        let temp_path = temp.path.clone();

        // Flush both WALs and close the temp connection before touching files.
        temp.checkpoint()?;
        drop(temp);
        self.checkpoint()?;
        self.wallet = None;
        self.persister = None;

        remove_sqlite_sidecars(&self.path);
        std::fs::rename(&temp_path, &self.path)?;
        rename_sqlite_sidecars(&temp_path, &self.path);

        // Rebuild the Wallet against the new inode/connection.
        let mut reopened =
            Self::open_with_backend(&self.path, &self.seed, self.network, self.chain.clone())?;

        // Never hand out a previously revealed-but-unused index again.
        let rebuilt_external = reopened
            .wallet()
            .next_derivation_index(KeychainKind::External);
        if rebuilt_external < previous_external {
            for _ in reopened
                .wallet_mut()
                .reveal_addresses_to(KeychainKind::External, previous_external - 1)
            {}
        }
        let rebuilt_internal = reopened
            .wallet()
            .next_derivation_index(KeychainKind::Internal);
        if rebuilt_internal < previous_internal {
            for _ in reopened
                .wallet_mut()
                .reveal_addresses_to(KeychainKind::Internal, previous_internal - 1)
            {}
        }
        reopened.persist()?;
        *self = reopened;
        Ok(())
    }

    fn checkpoint(&self) -> anyhow::Result<()> {
        self.persister
            .as_ref()
            .expect("BDK persister is present")
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }

    fn persist(&mut self) -> anyhow::Result<()> {
        let Self {
            wallet, persister, ..
        } = self;
        wallet
            .as_mut()
            .expect("BDK wallet is present")
            .persist(persister.as_mut().expect("BDK persister is present"))?;
        Ok(())
    }
}

fn remove_sqlite_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    remove_sqlite_sidecars(path);
}

fn remove_sqlite_sidecars(path: &Path) {
    let _ = std::fs::remove_file(sqlite_sidecar(path, "-wal"));
    let _ = std::fs::remove_file(sqlite_sidecar(path, "-shm"));
}

fn rename_sqlite_sidecars(from: &Path, to: &Path) {
    for suffix in ["-wal", "-shm"] {
        let source = sqlite_sidecar(from, suffix);
        if source.exists() {
            let _ = std::fs::rename(source, sqlite_sidecar(to, suffix));
        }
    }
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}{suffix}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::mock::{MockBackend, MockFailure};
    use crate::chain::{ChainKind, ChainTip};
    use bdk_chain::{BlockId, CheckPoint};
    use bdk_wallet::Update;
    use bitcoin::BlockHash;
    use bitcoin::hashes::Hash as _;
    use std::collections::BTreeMap;

    fn hash(byte: u8) -> BlockHash {
        BlockHash::from_byte_array([byte; 32])
    }

    struct TempWallet {
        dir: std::path::PathBuf,
        wallet: BdkWallet,
        backend: Arc<MockBackend>,
    }

    impl Drop for TempWallet {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// A wallet whose stored checkpoint is `(height, hash)` on regtest, plus a
    /// mock backend that we can point anywhere.
    fn wallet_at(
        kind: ChainKind,
        height: u32,
        local_hash: BlockHash,
        tip: ChainTip,
        remote_at_local: Option<BlockHash>,
    ) -> TempWallet {
        let dir =
            std::env::temp_dir().join(format!("berlin-wallet-chain-{}", uuid::Uuid::now_v7()));
        let path = dir.join("bdk.sqlite");
        let seed = [0x22u8; 64];
        let backend = Arc::new(MockBackend::new(kind, tip, remote_at_local));
        let chain: Arc<dyn ChainBackend> = backend.clone();
        let mut wallet = BdkWallet::open_with_backend(&path, &seed, Network::Regtest, chain)
            .expect("open wallet");

        let genesis = wallet.wallet().latest_checkpoint().block_id();
        let mut ids = vec![genesis];
        if height > 0 {
            ids.push(BlockId {
                height,
                hash: local_hash,
            });
        }
        let checkpoint = CheckPoint::from_block_ids(ids).expect("checkpoint");
        wallet
            .wallet_mut()
            .apply_update(Update {
                last_active_indices: BTreeMap::new(),
                tx_update: Default::default(),
                chain: Some(checkpoint),
            })
            .expect("apply checkpoint");
        assert_eq!(wallet.wallet().latest_checkpoint().height(), height);

        TempWallet {
            dir,
            wallet,
            backend,
        }
    }

    #[test]
    fn ancestry_decision_accepts_same_hash() {
        assert_eq!(
            reorg_decision(100, &hash(1), 120, Some(hash(1))),
            ReorgDecision::AncestorOk
        );
    }

    #[test]
    fn ancestry_decision_rejects_lower_remote_tip() {
        assert_eq!(
            reorg_decision(100, &hash(1), 99, None),
            ReorgDecision::Orphaned
        );
    }

    #[test]
    fn ancestry_decision_rejects_conflicting_hash() {
        assert_eq!(
            reorg_decision(100, &hash(1), 100, Some(hash(2))),
            ReorgDecision::Orphaned
        );
    }

    #[tokio::test]
    async fn a_conflicting_hash_rebuilds_for_every_backend() {
        for kind in [ChainKind::Esplora, ChainKind::Electrum, ChainKind::Bitcoind] {
            let mut fixture = wallet_at(
                kind,
                100,
                hash(1),
                ChainTip {
                    height: 120,
                    hash: hash(9),
                },
                Some(hash(2)),
            );
            fixture.wallet.sync().await.expect("sync");
            assert_eq!(
                fixture.backend.full_scan_calls(),
                1,
                "{kind}: an orphaned checkpoint must trigger a rebuild"
            );
            assert_eq!(
                fixture.backend.sync_calls(),
                0,
                "{kind}: the incremental update must not be trusted"
            );
        }
    }

    #[tokio::test]
    async fn a_lower_remote_tip_rebuilds_for_every_backend() {
        for kind in [ChainKind::Esplora, ChainKind::Electrum, ChainKind::Bitcoind] {
            let mut fixture = wallet_at(
                kind,
                100,
                hash(1),
                ChainTip {
                    height: 99,
                    hash: hash(9),
                },
                None,
            );
            fixture.wallet.sync().await.expect("sync");
            assert_eq!(fixture.backend.full_scan_calls(), 1, "{kind}");
            assert_eq!(fixture.backend.sync_calls(), 0, "{kind}");
        }
    }

    #[tokio::test]
    async fn an_intact_chain_syncs_without_a_rebuild() {
        for kind in [ChainKind::Esplora, ChainKind::Electrum, ChainKind::Bitcoind] {
            let mut fixture = wallet_at(
                kind,
                100,
                hash(1),
                ChainTip {
                    height: 120,
                    hash: hash(9),
                },
                Some(hash(1)),
            );
            fixture.wallet.sync().await.expect("sync");
            assert_eq!(fixture.backend.full_scan_calls(), 0, "{kind}");
            assert_eq!(fixture.backend.sync_calls(), 1, "{kind}");
        }
    }

    #[tokio::test]
    async fn transient_backend_errors_are_retried() {
        let mut fixture = wallet_at(
            ChainKind::Esplora,
            0,
            hash(0),
            ChainTip {
                height: 5,
                hash: hash(5),
            },
            None,
        );
        // At height 0 the checkpoint is the wallet's genesis; point the mock's
        // hash-at-local-height at it so the ancestry check passes.
        let genesis = fixture.wallet.wallet().latest_checkpoint().hash();
        fixture.backend.set_remote_at_local(Some(genesis));
        fixture
            .backend
            .queue_tip_failures([MockFailure::Transient, MockFailure::Transient]);
        fixture.wallet.sync().await.expect("sync");
        assert_eq!(fixture.backend.tip_calls(), 3);
        assert_eq!(fixture.backend.sync_calls(), 1);
    }

    #[tokio::test]
    async fn permanent_backend_errors_are_not_retried() {
        let mut fixture = wallet_at(
            ChainKind::Esplora,
            0,
            hash(0),
            ChainTip {
                height: 5,
                hash: hash(5),
            },
            Some(hash(1)),
        );
        fixture.backend.queue_tip_failures([MockFailure::Permanent]);
        let err = fixture.wallet.sync().await.expect_err("permanent failure");
        assert!(err.to_string().contains("permanent"), "unexpected: {err:#}");
        assert_eq!(fixture.backend.tip_calls(), 1);
    }

    #[test]
    fn swap_reopens_connection_and_preserves_next_index() {
        let base =
            std::env::temp_dir().join(format!("berlin-wallet-bdk-swap-{}", uuid::Uuid::now_v7()));
        let source_path = base.join("source.sqlite");
        let temp_path = base.join("temp.sqlite");
        let seed = [0x44u8; 64];
        let chain: ChainBackendConfig = "http://127.0.0.1:1".parse().expect("config");

        let mut wallet =
            BdkWallet::open_or_create(&source_path, &seed, Network::Regtest, chain.clone())
                .unwrap();
        wallet.reveal_destination().unwrap();
        wallet.reveal_destination().unwrap();
        let previous = wallet
            .wallet()
            .next_derivation_index(KeychainKind::External);

        let temp = BdkWallet::open_or_create(&temp_path, &seed, Network::Regtest, chain).unwrap();
        wallet.swap_in_scanned_db(temp).expect("swap");

        assert!(
            wallet
                .wallet()
                .next_derivation_index(KeychainKind::External)
                >= previous,
            "next index must not regress"
        );
        // This is the regression: the old connection pointed at the removed
        // inode and returned SQLite error 1032 on this write.
        wallet
            .reveal_destination()
            .expect("write on the reopened connection");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn lower_tip_update_is_a_noop_in_bdk() {
        use bdk_chain::local_chain::LocalChain;

        let blocks: BTreeMap<u32, BlockHash> = (0..=5).map(|i| (i, hash(i as u8 + 1))).collect();
        let mut local = LocalChain::from_blocks(blocks).unwrap();
        let update = CheckPoint::from_block_ids((0..=2).map(|i| BlockId {
            height: i,
            hash: hash(i as u8 + 1),
        }))
        .unwrap();
        let changeset = local.apply_update(update).unwrap();
        assert!(
            changeset.blocks.is_empty(),
            "lower-tip update must be a no-op"
        );
        assert_eq!(local.tip().height(), 5);
    }
}
