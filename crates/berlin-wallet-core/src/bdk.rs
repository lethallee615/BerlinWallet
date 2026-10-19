use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bdk_esplora::EsploraAsyncExt;
pub use bdk_wallet::KeychainKind;
use bdk_wallet::descriptor::IntoWalletDescriptor;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::signer::{SignOptions, SignersContainer};
use bdk_wallet::{PersistedWallet, Wallet};
use bitcoin::Network;
use bitcoin::bip32::Xpriv;

/// SQLite file used by the BDK wallet. The CLI places it under its data dir.
pub const BDK_DB_FILE: &str = "bdk.sqlite";

/// Bound each Esplora HTTP operation so a hung endpoint cannot wedge a task.
const ESPLORA_TIMEOUT: Duration = Duration::from_secs(30);
/// A restore scan has to walk historical gap limits; give it a much larger
/// bound than a normal incremental sync.
const ESPLORA_FULL_SCAN_TIMEOUT: Duration = Duration::from_secs(600);
const FULL_SCAN_STOP_GAP: usize = 20;
const FULL_SCAN_PARALLEL_REQUESTS: usize = 4;
const SYNC_MAX_ATTEMPTS: u32 = 3;
const SYNC_BACKOFF_BASE: Duration = Duration::from_millis(250);

#[derive(Debug)]
enum SyncAttemptError {
    Transient(anyhow::Error),
    Permanent(anyhow::Error),
}

impl SyncAttemptError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Transient(err) | Self::Permanent(err) => err,
        }
    }
}

fn is_transient_esplora(err: &bdk_esplora::esplora_client::Error) -> bool {
    use bdk_esplora::esplora_client::Error;
    match err {
        // Connection refused, DNS failure, and request timeouts are retryable.
        // Malformed bodies/parsing and HTTP status errors are surfaced normally.
        Error::Reqwest(err) => err.is_connect() || err.is_timeout() || err.is_request(),
        _ => false,
    }
}

impl From<bdk_esplora::esplora_client::Error> for SyncAttemptError {
    fn from(err: bdk_esplora::esplora_client::Error) -> Self {
        if is_transient_esplora(&err) {
            Self::Transient(anyhow::anyhow!(err))
        } else {
            Self::Permanent(anyhow::anyhow!(err))
        }
    }
}

fn classify_anyhow(err: anyhow::Error) -> SyncAttemptError {
    let transient = err
        .downcast_ref::<bdk_esplora::esplora_client::Error>()
        .is_some_and(is_transient_esplora)
        || err
            .downcast_ref::<Box<bdk_esplora::esplora_client::Error>>()
            .is_some_and(|err| is_transient_esplora(err));
    if transient {
        SyncAttemptError::Transient(err)
    } else {
        SyncAttemptError::Permanent(err)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReorgDecision {
    AncestorOk,
    Orphaned,
}

/// Decide whether the wallet's stored checkpoint is still on the remote chain.
///
/// `remote_at_local` is `/block-height/{local_height}` when the remote tip is at
/// least that high; when the remote tip is lower, the stored tip cannot be an
/// ancestor and the wallet must roll back/rescan.
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

/// BDK wallet plus its SQLite persister and software signers.
///
/// The wrapper exists because `PersistedWallet<Connection>` needs the
/// `Connection` alongside it for `persist`, and the caller should not have to
/// duplicate descriptor/signer construction.
pub struct BdkWallet {
    wallet: Option<PersistedWallet<Connection>>,
    persister: Option<Connection>,
    external_signers: SignersContainer,
    internal_signers: SignersContainer,
    esplora_url: String,
    path: std::path::PathBuf,
    seed: [u8; 64],
    network: Network,
}

impl BdkWallet {
    /// Open `path` or create it with BIP84 descriptors derived from `seed`.
    pub fn open_or_create(
        path: &Path,
        seed: &[u8; 64],
        network: Network,
        esplora_url: impl Into<String>,
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
            esplora_url: esplora_url.into(),
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

    /// Build, sign, finalize and record an unconfirmed payment.
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
        let esplora_url = self.esplora_url.clone();
        Self::broadcast_url(&esplora_url, &tx).await?;
        let last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.wallet_mut()
            .apply_unconfirmed_txs([(tx.clone(), last_seen)]);
        self.persist()?;
        Ok(tx)
    }

    /// Full script-pubkey scan from index 0 for a restored wallet.
    ///
    /// `start_full_scan()` uses the wallet's unbounded script iterators, so it
    /// starts at derivation index 0 even on a fresh BDK database. A partial
    /// `sync()` only covers already-revealed scripts and would miss historical
    /// addresses.
    pub async fn full_scan(&mut self) -> anyhow::Result<Vec<(KeychainKind, u32)>> {
        let request = self.wallet().start_full_scan();
        let client = bdk_esplora::esplora_client::Builder::new(&self.esplora_url).build_async()?;
        let response = tokio::time::timeout(
            ESPLORA_FULL_SCAN_TIMEOUT,
            client.full_scan(request, FULL_SCAN_STOP_GAP, FULL_SCAN_PARALLEL_REQUESTS),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Esplora full scan timed out"))??;
        let last_active = response
            .last_active_indices
            .iter()
            .map(|(keychain, index)| (keychain.clone(), *index))
            .collect();
        self.wallet_mut().apply_update(response)?;
        self.persist()?;
        Ok(last_active)
    }

    /// Reorg-safe partial Esplora sync.
    ///
    /// `bdk_esplora` builds its chain update from the wallet's local tip. If the
    /// remote tip is at or below that tip, the update can be a no-op and leave
    /// anchors to orphaned blocks (see COMPILE.md §34). We verify ancestry
    /// first and, when orphaned, rebuild the wallet DB and full-scan instead of
    /// trusting the incremental update.
    pub async fn sync(&mut self) -> anyhow::Result<()> {
        let client = bdk_esplora::esplora_client::Builder::new(&self.esplora_url)
            .build_async()
            .map_err(|e| anyhow::anyhow!(e))?;
        let mut backoff = SYNC_BACKOFF_BASE;
        let mut last_error = None;

        for attempt in 1..=SYNC_MAX_ATTEMPTS {
            match self.sync_attempt(&client).await {
                Ok(()) => return Ok(()),
                Err(SyncAttemptError::Permanent(err)) => return Err(err),
                Err(error @ SyncAttemptError::Transient(_)) => {
                    last_error = Some(error.into_anyhow());
                    if attempt < SYNC_MAX_ATTEMPTS {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(ESPLORA_TIMEOUT);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Esplora sync failed")))
    }

    async fn sync_attempt(
        &mut self,
        client: &bdk_esplora::esplora_client::AsyncClient,
    ) -> std::result::Result<(), SyncAttemptError> {
        let local_tip = self.wallet().latest_checkpoint();
        let local_height = local_tip.height();
        let remote_height = client.get_height().await.map_err(SyncAttemptError::from)?;
        let remote_tip_hash = client
            .get_tip_hash()
            .await
            .map_err(SyncAttemptError::from)?;
        let remote_at_local = if remote_height >= local_height {
            Some(
                client
                    .get_block_hash(local_height)
                    .await
                    .map_err(SyncAttemptError::from)?,
            )
        } else {
            None
        };

        if reorg_decision(
            local_height,
            &local_tip.hash(),
            remote_height,
            remote_at_local,
        ) == ReorgDecision::Orphaned
        {
            tracing::info!(
                local_height,
                remote_height,
                remote_tip = ?remote_tip_hash,
                "BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan"
            );
            // The incremental update cannot lower the local tip (merge_chains
            // returns an empty changeset when the update tip is lower and all
            // overlapping hashes agree). Rebuild from scratch and full-scan.
            self.reset_and_full_scan().await.map_err(classify_anyhow)?;
            return Ok(());
        }

        let request = self.wallet().start_sync_with_revealed_spks();
        let update = tokio::time::timeout(ESPLORA_TIMEOUT, client.sync(request, 4))
            .await
            .map_err(|_| SyncAttemptError::Transient(anyhow::anyhow!("Esplora sync timed out")))?
            .map_err(|err| SyncAttemptError::from(*err))?;
        self.wallet_mut()
            .apply_update(update)
            .map_err(|err| SyncAttemptError::Permanent(anyhow::anyhow!(err)))?;
        self.persist().map_err(SyncAttemptError::Permanent)?;
        Ok(())
    }

    /// Rebuild the BDK wallet from the same seed/descriptors and full-scan.
    ///
    /// Used only when the stored checkpoint is orphaned and the incremental
    /// update cannot force the tip down. The old DB is replaced only after the
    /// fresh wallet has successfully full-scanned.
    async fn reset_and_full_scan(&mut self) -> anyhow::Result<()> {
        let temp_path = self.path.with_file_name(format!(
            "{}.reorg-{}",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("bdk.sqlite"),
            uuid::Uuid::now_v7()
        ));
        let mut fresh = Self::open_or_create(
            &temp_path,
            &self.seed,
            self.network,
            self.esplora_url.clone(),
        )?;
        if let Err(err) = fresh.full_scan().await {
            remove_sqlite_files(&temp_path);
            return Err(err);
        }

        self.swap_in_scanned_db(fresh)
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
        let mut reopened = Self::open_or_create(
            &self.path,
            &self.seed,
            self.network,
            self.esplora_url.clone(),
        )?;

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

    async fn broadcast_url(esplora_url: &str, tx: &bitcoin::Transaction) -> anyhow::Result<()> {
        let client = bdk_esplora::esplora_client::Builder::new(esplora_url).build_async()?;
        tokio::time::timeout(ESPLORA_TIMEOUT, client.broadcast(tx))
            .await
            .map_err(|_| anyhow::anyhow!("Esplora broadcast timed out"))??;
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
    use bitcoin::BlockHash;
    use bitcoin::hashes::Hash as _;

    fn hash(byte: u8) -> BlockHash {
        BlockHash::from_byte_array([byte; 32])
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

    #[test]
    fn http_4xx_is_a_permanent_sync_error() {
        let error = bdk_esplora::esplora_client::Error::HttpResponse {
            status: 400,
            message: "bad request".to_owned(),
        };
        assert!(!is_transient_esplora(&error));
    }

    #[test]
    fn malformed_body_is_a_permanent_sync_error() {
        let error =
            bdk_esplora::esplora_client::Error::Parsing("not-a-number".parse::<u32>().unwrap_err());
        assert!(!is_transient_esplora(&error));
    }

    #[test]
    fn swap_reopens_connection_and_preserves_next_index() {
        let base =
            std::env::temp_dir().join(format!("berlin-wallet-bdk-swap-{}", uuid::Uuid::now_v7()));
        let source_path = base.join("source.sqlite");
        let temp_path = base.join("temp.sqlite");
        let seed = [0x44u8; 64];
        let url = "http://127.0.0.1:1";

        let mut wallet =
            BdkWallet::open_or_create(&source_path, &seed, Network::Regtest, url).unwrap();
        wallet.reveal_destination().unwrap();
        wallet.reveal_destination().unwrap();
        let previous = wallet
            .wallet()
            .next_derivation_index(KeychainKind::External);

        let temp = BdkWallet::open_or_create(&temp_path, &seed, Network::Regtest, url).unwrap();
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
        use bdk_chain::{BlockId, CheckPoint};
        use std::collections::BTreeMap;

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
