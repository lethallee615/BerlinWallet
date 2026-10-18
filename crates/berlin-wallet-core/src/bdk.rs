use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bdk_esplora::EsploraAsyncExt;
use bdk_wallet::descriptor::IntoWalletDescriptor;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::signer::{SignOptions, SignersContainer};
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};
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
    wallet: PersistedWallet<Connection>,
    persister: Connection,
    external_signers: SignersContainer,
    internal_signers: SignersContainer,
    esplora_url: String,
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
            wallet,
            persister,
            external_signers,
            internal_signers,
            esplora_url: esplora_url.into(),
        })
    }

    pub fn network(&self) -> Network {
        self.wallet.network()
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.wallet.balance()
    }

    /// True if `address` belongs to this BDK wallet. Only in that case could a
    /// local script scan observe a withdrawal output; external destinations are
    /// reconciled from the federation's `Success(txid)` instead.
    pub fn is_our_address(
        &self,
        address: &bitcoin::Address<bitcoin::address::NetworkUnchecked>,
    ) -> bool {
        self.wallet
            .is_mine(address.clone().assume_checked().script_pubkey())
    }

    /// Reveal and persist the next external address.
    pub fn reveal_destination(&mut self) -> anyhow::Result<bitcoin::Address> {
        let address = self
            .wallet
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
        let mut builder = self.wallet.build_tx();
        builder.add_recipient(destination.script_pubkey(), amount);
        let mut psbt = builder.finish()?;

        let finalized = self.wallet.sign_with_signers(
            &mut psbt,
            &[&self.external_signers, &self.internal_signers],
            SignOptions::default(),
        )?;
        anyhow::ensure!(finalized, "BDK could not finalize the PSBT");

        let tx = psbt.extract_tx()?;
        self.broadcast(&tx).await?;
        let last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.wallet.apply_unconfirmed_txs([(tx.clone(), last_seen)]);
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
        let request = self.wallet.start_full_scan();
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
        self.wallet.apply_update(response)?;
        self.persist()?;
        Ok(last_active)
    }

    /// One partial Esplora sync over the revealed script pubkeys, then persist
    /// the new checkpoint and transaction data.
    pub async fn sync(&mut self) -> anyhow::Result<()> {
        let request = self.wallet.start_sync_with_revealed_spks();
        let client = bdk_esplora::esplora_client::Builder::new(&self.esplora_url).build_async()?;
        let update = tokio::time::timeout(ESPLORA_TIMEOUT, client.sync(request, 4))
            .await
            .map_err(|_| anyhow::anyhow!("Esplora sync timed out"))??;
        self.wallet.apply_update(update)?;
        self.persist()?;
        Ok(())
    }

    async fn broadcast(&self, tx: &bitcoin::Transaction) -> anyhow::Result<()> {
        let client = bdk_esplora::esplora_client::Builder::new(&self.esplora_url).build_async()?;
        tokio::time::timeout(ESPLORA_TIMEOUT, client.broadcast(tx))
            .await
            .map_err(|_| anyhow::anyhow!("Esplora broadcast timed out"))??;
        Ok(())
    }

    fn persist(&mut self) -> anyhow::Result<()> {
        self.wallet.persist(&mut self.persister)?;
        Ok(())
    }
}
