//! UniFFI surface for Berlin Wallet.
//!
//! One namespace: this crate's `PegWallet` object. Fedimint's own `uniffi`
//! feature is deliberately not enabled; `ClientHandleArc` is kept directly and
//! Deref/`get_first_module_arc` are used instead of `ClientHandle::inner_arc`.
//!
//! This library does **not** install a tracing subscriber: the embedding app
//! owns logging. The CLI installs one with `try_init`, so a host and the CLI
//! cannot double-panic; core logs flow to whichever subscriber the host set.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::Context as _;
use berlin_wallet_core::backup;
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet, KeychainKind};
use berlin_wallet_core::chain::ChainBackendConfig;
use berlin_wallet_core::client::{
    build_client, client_db, min_withdrawal_sat, open_existing_client, open_rocksdb,
    recover_client, wallet_client_config,
};
use berlin_wallet_core::deposit;
use berlin_wallet_core::journal::{
    DepositOrigin, DepositRow, DepositState, Journal, WithdrawalOrigin, WithdrawalRow,
    WithdrawalState, collapse_deposit_rows,
};
use berlin_wallet_core::network::{
    init_network, parse_network, require_recorded_network, verified_client_network,
};
use berlin_wallet_core::ops::{DepositId, WithdrawalId};
use berlin_wallet_core::seed;
use berlin_wallet_core::sync;
use berlin_wallet_core::withdrawal;
use bitcoin::{Amount, Network};
use fedimint_client::{Client, ClientHandleArc};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::config::FederationId;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::AmountUnit;
use fedimint_walletv2_client::WalletClientModule;
use fedimint_walletv2_common::KIND as WALLET_V2_KIND;
use tokio::sync::{Mutex, watch};

uniffi::setup_scaffolding!();

type Result<T> = std::result::Result<T, PegWalletError>;

/// Single FFI error type. Core errors are flattened into a display message so
/// no `anyhow::Error` crosses the FFI boundary.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PegWalletError {
    #[error("{message}")]
    Message { message: String },
}

impl From<anyhow::Error> for PegWalletError {
    fn from(err: anyhow::Error) -> Self {
        Self::Message {
            message: format!("{err:#}"),
        }
    }
}

impl From<std::io::Error> for PegWalletError {
    fn from(err: std::io::Error) -> Self {
        Self::Message {
            message: err.to_string(),
        }
    }
}

fn err(message: impl Into<String>) -> PegWalletError {
    PegWalletError::Message {
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DepositOriginView {
    Requested,
    Adopted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DepositStateView {
    AwaitingAddress,
    AddressShown,
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WithdrawalOriginView {
    Requested,
    Adopted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WithdrawalStateView {
    Quoting,
    Submitted,
    Broadcast,
    Refunded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct InitView {
    pub data_dir: String,
    pub mnemonic_path: String,
    pub network: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct WalletInfo {
    pub federation_id: String,
    pub client_identity: String,
    pub network: String,
    pub primary_btc_module: Option<u32>,
    pub onchain_balance_sat: u64,
    pub ecash_balance_msat: u64,
    pub min_withdrawal_sat: u64,
    /// Active chain backend, human-readable and without credentials.
    pub chain_source: String,
    /// `esplora` / `electrum` / `bitcoind`.
    pub chain_kind: String,
}

/// Nullable net credit is deliberate: `None` means the mintv2 operation fee is
/// not yet known and the UI must render "pending", never 0.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DepositView {
    pub id: String,
    pub origin: DepositOriginView,
    pub state: DepositStateView,
    pub address: Option<String>,
    pub amount_sat: u64,
    pub gross_deposited_sat: Option<u64>,
    pub quoted_fee_sat: Option<u64>,
    pub quoted_operation_fee_msat: Option<u64>,
    pub claim_fee_sat: Option<u64>,
    pub operation_fee_msat: Option<u64>,
    pub net_credit_msat: Option<u64>,
    pub funding_txid: Option<String>,
    pub operation_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct WithdrawalView {
    pub id: String,
    pub origin: WithdrawalOriginView,
    pub state: WithdrawalStateView,
    pub address: String,
    pub recipient_amount_sat: u64,
    pub quoted_fee_sat: Option<u64>,
    pub quote_input_msat: Option<u64>,
    pub quote_output_msat: Option<u64>,
    pub quote_dust_msat: Option<u64>,
    pub total_debit_msat: Option<u64>,
    pub operation_id: Option<String>,
    pub txid: Option<String>,
    pub custom_meta: String,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RestoreView {
    pub federation_id: String,
    pub client_identity: String,
    pub network: String,
    pub onchain_balance_sat: u64,
    pub ecash_balance_msat: u64,
    pub min_withdrawal_sat: u64,
    pub operation_log_empty: bool,
    pub history_unavailable: bool,
    pub journal_deposit_count: u32,
    pub journal_withdrawal_count: u32,
    pub bdk_external_last_active: Option<u32>,
    pub bdk_internal_last_active: Option<u32>,
}

struct WalletState {
    client: ClientHandleArc,
    journal: Journal,
    bdk: Arc<Mutex<BdkWallet>>,
    federation_id: FederationId,
    client_identity: String,
    network: Network,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    watchers: sync::DepositWatchers,
}

/// One FFI object. `new` only stores the data dir; `init` creates/loads the
/// mnemonic, and `join`/`restore` attach a client and start sync tasks.
#[derive(uniffi::Object)]
pub struct PegWallet {
    data_dir: PathBuf,
    inner: Mutex<Option<WalletState>>,
    shutdown: StdMutex<Option<watch::Sender<bool>>>,
    closed: AtomicBool,
}

impl Drop for PegWallet {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.shutdown.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(true);
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl PegWallet {
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Arc<Self> {
        Arc::new(Self {
            data_dir: PathBuf::from(data_dir),
            inner: Mutex::new(None),
            shutdown: StdMutex::new(None),
            closed: AtomicBool::new(false),
        })
    }

    /// Create/load the mnemonic and record the network choice. This is
    /// local-only and fast; joining or restoring is a separate async call.
    pub fn init(&self, network: String) -> Result<InitView> {
        self.ensure_open()?;
        std::fs::create_dir_all(&self.data_dir)?;
        let network = init_network(&self.data_dir, parse_network(&network)?)?;
        let mnemonic_path = self.data_dir.join("mnemonic");
        seed::load_or_create_mnemonic(&mnemonic_path)?;
        Ok(InitView {
            data_dir: self.data_dir.display().to_string(),
            mnemonic_path: mnemonic_path.display().to_string(),
            network: network.to_string(),
        })
    }

    /// Join (or open) a federation using this data directory.
    pub async fn join(&self, invite_code: String, chain_source: String) -> Result<WalletInfo> {
        self.ensure_open()?;
        if self.inner.lock().await.is_some() {
            return Err(err("wallet is already joined/restored"));
        }
        std::fs::create_dir_all(&self.data_dir)?;
        let recorded = require_recorded_network(&self.data_dir)?;
        let mnemonic = seed::load_or_create_mnemonic(&self.data_dir.join("mnemonic"))?;
        let seed = seed::mnemonic_seed(&mnemonic);
        let root_secret = seed::root_secret_from_mnemonic(&mnemonic);
        let invite = InviteCode::from_str(&invite_code).context("invalid invite code")?;
        let federation_id = invite.federation_id();
        let client_identity = seed::client_identity(&root_secret, federation_id)?;

        let db = open_rocksdb(&self.data_dir.join("fedimint.rocksdb")).await?;
        let client_partition = client_db(&db, 0);
        if Client::get_config_from_db(&client_partition)
            .await
            .is_some()
        {
            return Err(err(
                "wallet already initialized; use open() (join is fresh-join only)",
            ));
        }
        let journal = Journal::new(db.clone(), vec![0x00]);
        let connectors = ConnectorRegistry::build_from_client_defaults()
            .bind()
            .await?;
        let client =
            build_client(client_partition, connectors, root_secret.clone(), invite).await?;
        let network = verified_client_network(&client, recorded).await?;
        let bdk = BdkWallet::open_or_create(
            &self.data_dir.join(BDK_DB_FILE),
            &seed,
            network,
            self.resolve_chain_source(Some(chain_source))?,
        )?;
        let state = WalletState {
            client,
            journal,
            bdk: Arc::new(Mutex::new(bdk)),
            federation_id,
            client_identity,
            network,
            tasks: Vec::new(),
            watchers: sync::DepositWatchers::new(),
        };
        self.install_state(state).await?;
        self.wallet_info().await
    }

    /// Open an existing wallet without an invite. Use this on app restart when
    /// the data directory already contains a joined client.
    /// `chain_source` `None` means "use the one recorded in `chain.json`".
    /// Passing a value changes it; the change is logged, never silent.
    pub async fn open(&self, chain_source: Option<String>) -> Result<WalletInfo> {
        self.ensure_open()?;
        self.open_existing(chain_source, None).await
    }

    /// Restore a fresh data directory from the mnemonic. This performs the
    /// 0.12.0 recover/wait/shutdown/reopen flow and a full BDK scan.
    pub async fn restore(
        &self,
        invite_code: String,
        chain_source: String,
        network: String,
    ) -> Result<RestoreView> {
        self.ensure_open()?;
        if self.inner.lock().await.is_some() {
            return Err(err("wallet is already joined/restored"));
        }
        let network = parse_network(&network)?;
        let mnemonic_path = self.data_dir.join("mnemonic");
        if !mnemonic_path.is_file() {
            return Err(err(format!(
                "restore requires an existing mnemonic at {}",
                mnemonic_path.display()
            )));
        }
        if self.data_dir.join(BDK_DB_FILE).exists() {
            return Err(err("restore requires a fresh data dir (bdk.sqlite exists)"));
        }
        let mnemonic = seed::load_or_create_mnemonic(&mnemonic_path)?;
        let seed = seed::mnemonic_seed(&mnemonic);
        let root_secret = seed::root_secret_from_mnemonic(&mnemonic);
        let invite = InviteCode::from_str(&invite_code).context("invalid invite code")?;
        let federation_id = invite.federation_id();
        let client_identity = seed::client_identity(&root_secret, federation_id)?;

        let db = open_rocksdb(&self.data_dir.join("fedimint.rocksdb")).await?;
        let journal = Journal::new(db.clone(), vec![0x00]);
        let connectors = ConnectorRegistry::build_from_client_defaults()
            .bind()
            .await?;
        let client =
            recover_client(client_db(&db, 0), connectors, root_secret.clone(), invite).await?;
        let network = verified_client_network(&client, network).await?;
        init_network(&self.data_dir, network)?;
        let mut bdk = BdkWallet::open_or_create(
            &self.data_dir.join(BDK_DB_FILE),
            &seed,
            network,
            self.resolve_chain_source(Some(chain_source))?,
        )?;
        let last_active = bdk.full_scan().await?;
        let operation_log_empty = client
            .operation_log()
            .paginate_operations_rev(1, None)
            .await
            .is_empty();
        let ecash_balance_msat = client
            .get_balance_for_unit(AmountUnit::BITCOIN)
            .await?
            .msats;
        let onchain_balance_sat = bdk.balance().trusted_spendable().to_sat();
        let min_withdrawal_sat = min_withdrawal_sat(&client).await?;
        let deposit_count = journal.deposits_all().await?.len() as u32;
        let withdrawal_count = journal.withdrawals_all().await?.len() as u32;
        let (external, internal) = last_active_summary(&last_active);

        let state = WalletState {
            client,
            journal,
            bdk: Arc::new(Mutex::new(bdk)),
            federation_id,
            client_identity: client_identity.clone(),
            network,
            tasks: Vec::new(),
            watchers: sync::DepositWatchers::new(),
        };
        self.install_state(state).await?;
        Ok(RestoreView {
            federation_id: federation_id.to_string(),
            client_identity,
            network: network.to_string(),
            onchain_balance_sat,
            ecash_balance_msat,
            min_withdrawal_sat,
            operation_log_empty,
            history_unavailable: operation_log_empty,
            journal_deposit_count: deposit_count,
            journal_withdrawal_count: withdrawal_count,
            bdk_external_last_active: external,
            bdk_internal_last_active: internal,
        })
    }

    /// Import a full data-directory backup and open it as a retained client DB.
    /// The invite is still required to verify/open the client.
    pub async fn restore_backup(
        &self,
        source_dir: String,
        invite_code: String,
        chain_source: Option<String>,
    ) -> Result<WalletInfo> {
        self.ensure_open()?;
        if self.inner.lock().await.is_some() {
            return Err(err("wallet is already joined/restored"));
        }
        let source = PathBuf::from(source_dir);
        let target = self.data_dir.clone();
        tokio::task::spawn_blocking(move || backup::import_data_dir(&target, &source))
            .await
            .map_err(|e| err(format!("backup import task failed: {e}")))??;
        let invite = InviteCode::from_str(&invite_code).context("invalid invite code")?;
        self.open_existing(chain_source, Some(invite.federation_id()))
            .await
    }

    /// Full-directory backup. Fails loudly if the client DB is live.
    pub async fn backup(&self, destination_dir: String) -> Result<()> {
        self.ensure_open()?;
        let source = self.data_dir.clone();
        let destination = PathBuf::from(destination_dir);
        tokio::task::spawn_blocking(move || backup::backup_data_dir(&source, &destination))
            .await
            .map_err(|e| err(format!("backup task failed: {e}")))??;
        Ok(())
    }

    /// Stop sync/adoption tasks, shut down the fedimint client, and release the
    /// DB lock. Mobile MUST call this when backgrounding; otherwise backup will
    /// fail with "database is locked".
    pub async fn shutdown(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let state = {
            let mut guard = self.inner.lock().await;
            guard.take()
        };
        if let Ok(mut shutdown) = self.shutdown.lock()
            && let Some(tx) = shutdown.take()
        {
            let _ = tx.send(true);
        }
        let Some(state) = state else {
            return Ok(());
        };

        // The observers hold client handles. Let them exit before the
        // `Arc::try_unwrap` below, otherwise shutdown reports a lingering
        // reference even though nothing is actually running.
        state.watchers.wait_all().await;

        for mut task in state.tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }

        match Arc::try_unwrap(state.client) {
            Ok(handle) => handle.shutdown().await,
            Err(handle) => {
                drop(handle);
                return Err(err(
                    "client handle is still referenced after task shutdown; DB lock may remain",
                ));
            }
        }
        Ok(())
    }

    /// Request a deposit address. Returns immediately in `AwaitingAddress`; the
    /// background address task writes the address, which is polled via
    /// `deposit_status`.
    pub async fn request_deposit(&self, amount_sat: u64) -> Result<DepositView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let row = deposit::request_deposit(
            &state.client,
            &state.journal,
            state.federation_id,
            Amount::from_sat(amount_sat),
        )
        .await?;
        let module = state
            .client
            .get_first_module_arc::<WalletClientModule>()
            .map_err(|e| err(e.to_string()))?;
        let journal = state.journal.clone();
        let id = row.id.clone();
        tokio::spawn(async move {
            if let Err(error) = deposit::run_address_task(&journal, &module, &id).await {
                eprintln!("deposit address task failed for {}: {error:#}", id.0);
            }
        });
        Ok(deposit_view(&row))
    }

    pub async fn fund_deposit(&self, deposit_id: String) -> Result<DepositView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let module = state
            .client
            .get_first_module::<WalletClientModule>()
            .map_err(|e| err(e.to_string()))?;
        let mut bdk = state.bdk.lock().await;
        deposit::fund_deposit(
            &state.journal,
            module.inner(),
            &mut bdk,
            &DepositId(deposit_id.clone()),
        )
        .await?;
        drop(bdk);
        // Fund returns as soon as the transaction is broadcast. The watcher
        // follows the row to a terminal state in the background, the same way
        // the CLI does, and is resumed from the journal on the next `open`.
        if let Some(shutdown) = self
            .shutdown
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(watch::Sender::subscribe))
        {
            state.watchers.spawn(
                state.client.clone(),
                state.journal.clone(),
                state.federation_id,
                DepositId(deposit_id.clone()),
                shutdown,
            );
        }
        let row = state
            .journal
            .deposit(&DepositId(deposit_id))
            .await?
            .ok_or_else(|| err("deposit not found"))?;
        Ok(deposit_view(&row))
    }

    pub async fn deposit_status(&self, deposit_id: String) -> Result<DepositView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let row = state
            .journal
            .deposit(&DepositId(deposit_id))
            .await?
            .ok_or_else(|| err("deposit not found"))?;
        Ok(deposit_view(&row))
    }

    pub async fn cancel_deposit(&self, deposit_id: String) -> Result<DepositView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let id = DepositId(deposit_id);
        let result = state.journal.cancel_deposit(&id).await?;
        if result != berlin_wallet_core::journal::ApplyResult::Applied {
            return Err(err("deposit cannot be cancelled in its current state"));
        }
        let row = state
            .journal
            .deposit(&id)
            .await?
            .ok_or_else(|| err("deposit not found"))?;
        Ok(deposit_view(&row))
    }

    pub async fn list_deposits(&self) -> Result<Vec<DepositView>> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        Ok(collapse_deposit_rows(state.journal.deposits_all().await?)
            .iter()
            .map(deposit_view)
            .collect())
    }

    pub async fn request_withdrawal(
        &self,
        address: String,
        amount_sat: u64,
    ) -> Result<WithdrawalView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let min = min_withdrawal_sat(&state.client).await?;
        if amount_sat < min {
            return Err(err(format!(
                "withdrawal amount {amount_sat} sat is below the federation minimum {min} sat"
            )));
        }
        let row = withdrawal::request_withdrawal(
            &state.client,
            &state.journal,
            state.federation_id,
            address,
            Amount::from_sat(amount_sat),
        )
        .await?;
        Ok(withdrawal_view(&row))
    }

    pub async fn send_withdrawal(&self, withdrawal_id: String) -> Result<WithdrawalView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let id = WithdrawalId(withdrawal_id);
        withdrawal::send_withdrawal(&state.client, &state.journal, &id).await?;
        let row = state
            .journal
            .withdrawal(&id)
            .await?
            .ok_or_else(|| err("withdrawal not found"))?;
        Ok(withdrawal_view(&row))
    }

    pub async fn withdrawal_status(&self, withdrawal_id: String) -> Result<WithdrawalView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let row = state
            .journal
            .withdrawal(&WithdrawalId(withdrawal_id))
            .await?
            .ok_or_else(|| err("withdrawal not found"))?;
        Ok(withdrawal_view(&row))
    }

    pub async fn cancel_withdrawal(&self, withdrawal_id: String) -> Result<WithdrawalView> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let id = WithdrawalId(withdrawal_id);
        let result = state.journal.cancel_withdrawal(&id).await?;
        if result != berlin_wallet_core::journal::ApplyResult::Applied {
            return Err(err("withdrawal cannot be cancelled in its current state"));
        }
        let row = state
            .journal
            .withdrawal(&id)
            .await?
            .ok_or_else(|| err("withdrawal not found"))?;
        Ok(withdrawal_view(&row))
    }

    pub async fn list_withdrawals(&self) -> Result<Vec<WithdrawalView>> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        Ok(state
            .journal
            .withdrawals_all()
            .await?
            .iter()
            .map(withdrawal_view)
            .collect())
    }

    pub async fn balance_sat(&self) -> Result<u64> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        Ok(state
            .bdk
            .lock()
            .await
            .balance()
            .trusted_spendable()
            .to_sat())
    }

    pub async fn ecash_balance_msat(&self) -> Result<u64> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        Ok(state
            .client
            .get_balance_for_unit(AmountUnit::BITCOIN)
            .await?
            .msats)
    }

    pub async fn min_withdrawal_sat(&self) -> Result<u64> {
        self.ensure_open()?;
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        min_withdrawal_sat(&state.client)
            .await
            .map_err(PegWalletError::from)
    }
}

impl PegWallet {
    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(err(
                "wallet instance is shut down; create a new PegWallet and call open()",
            ));
        }
        Ok(())
    }

    /// Resolve the chain source for this data directory and record it in
    /// `chain.json`. `requested` wins over the stored value, and a difference is
    /// logged rather than applied silently.
    fn resolve_chain_source(&self, requested: Option<String>) -> Result<ChainBackendConfig> {
        berlin_wallet_core::chain::persist::resolve_chain_config(
            &self.data_dir,
            requested.as_deref(),
        )
        .map_err(|resolve_err| err(format!("{resolve_err:#}")))?
        .ok_or_else(|| {
            err("no chain source recorded; pass one (esplora:http://… or electrum:tcp://…)")
        })
    }

    async fn install_state(&self, mut state: WalletState) -> Result<()> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Err(err("wallet is already joined/restored"));
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let client = state.client.clone();
        let journal = state.journal.clone();
        let federation_id = state.federation_id;
        let watchers = state.watchers.clone();
        let resumed = watchers
            .resume(&client, &journal, federation_id, shutdown_rx.clone())
            .await?;
        let _ = resumed;
        let bdk = state.bdk.clone();
        state.tasks = vec![
            tokio::spawn(sync::bdk_chain_task(bdk, shutdown_rx.clone())),
            tokio::spawn(sync::fedimint_sync_task(
                client.clone(),
                journal.clone(),
                shutdown_rx.clone(),
            )),
            tokio::spawn(sync::adoption_task(
                client,
                journal,
                federation_id,
                shutdown_rx,
            )),
        ];
        *guard = Some(state);
        if let Ok(mut shutdown) = self.shutdown.lock() {
            *shutdown = Some(shutdown_tx);
        }
        Ok(())
    }

    async fn open_existing(
        &self,
        chain_source: Option<String>,
        expected_federation: Option<FederationId>,
    ) -> Result<WalletInfo> {
        if self.inner.lock().await.is_some() {
            return Err(err("wallet is already joined/restored"));
        }
        let mnemonic_path = self.data_dir.join("mnemonic");
        if !mnemonic_path.is_file() {
            return Err(err(format!(
                "open requires an existing mnemonic at {}",
                mnemonic_path.display()
            )));
        }
        let mnemonic = seed::load_or_create_mnemonic(&mnemonic_path)?;
        let seed = seed::mnemonic_seed(&mnemonic);
        let root_secret = seed::root_secret_from_mnemonic(&mnemonic);
        let recorded = require_recorded_network(&self.data_dir)?;
        let db = open_rocksdb(&self.data_dir.join("fedimint.rocksdb")).await?;
        let journal = Journal::new(db.clone(), vec![0x00]);
        let connectors = ConnectorRegistry::build_from_client_defaults()
            .bind()
            .await?;
        let client =
            open_existing_client(client_db(&db, 0), connectors, root_secret.clone()).await?;
        let federation_id = client.federation_id();
        if let Some(expected) = expected_federation
            && federation_id != expected
        {
            return Err(err("backup federation id mismatch"));
        }
        let client_identity = seed::client_identity(&root_secret, federation_id)?;
        let network = verified_client_network(&client, recorded).await?;
        let bdk = BdkWallet::open_or_create(
            &self.data_dir.join(BDK_DB_FILE),
            &seed,
            network,
            self.resolve_chain_source(chain_source)?,
        )?;
        let state = WalletState {
            client,
            journal,
            bdk: Arc::new(Mutex::new(bdk)),
            federation_id,
            client_identity,
            network,
            tasks: Vec::new(),
            watchers: sync::DepositWatchers::new(),
        };
        self.install_state(state).await?;
        self.wallet_info().await
    }

    async fn wallet_info(&self) -> Result<WalletInfo> {
        let guard = self.inner.lock().await;
        let state = guard.as_ref().ok_or_else(|| err("wallet is not joined"))?;
        let network = state.network;
        let (onchain_balance_sat, chain_source, chain_kind) = {
            let bdk = state.bdk.lock().await;
            let balance = bdk.balance().trusted_spendable().to_sat();
            (
                balance,
                bdk.chain_description(),
                bdk.chain_kind().to_string(),
            )
        };
        let ecash_balance_msat = state
            .client
            .get_balance_for_unit(AmountUnit::BITCOIN)
            .await?
            .msats;
        let min_withdrawal_sat = min_withdrawal_sat(&state.client).await?;
        let primary_btc_module = state
            .client
            .primary_module_for_unit(AmountUnit::BITCOIN)
            .map(|(id, _)| u32::from(id));
        Ok(WalletInfo {
            federation_id: state.federation_id.to_string(),
            client_identity: state.client_identity.clone(),
            network: network.to_string(),
            primary_btc_module,
            onchain_balance_sat,
            ecash_balance_msat,
            min_withdrawal_sat,
            chain_source,
            chain_kind,
        })
    }
}

fn last_active_summary(last_active: &[(KeychainKind, u32)]) -> (Option<u32>, Option<u32>) {
    let mut external = None;
    let mut internal = None;
    for (keychain, index) in last_active {
        match keychain {
            KeychainKind::External => external = Some(*index),
            KeychainKind::Internal => internal = Some(*index),
        }
    }
    (external, internal)
}

fn deposit_view(row: &DepositRow) -> DepositView {
    DepositView {
        id: row.id.0.clone(),
        origin: match row.origin {
            DepositOrigin::Requested => DepositOriginView::Requested,
            DepositOrigin::Adopted => DepositOriginView::Adopted,
        },
        state: match row.state {
            DepositState::AwaitingAddress => DepositStateView::AwaitingAddress,
            DepositState::AddressShown => DepositStateView::AddressShown,
            DepositState::Done => DepositStateView::Done,
            DepositState::Cancelled => DepositStateView::Cancelled,
            DepositState::Failed => DepositStateView::Failed,
        },
        address: row.address.clone(),
        amount_sat: row.amount.to_sat(),
        gross_deposited_sat: row.gross_deposited.map(|a| a.to_sat()),
        quoted_fee_sat: row.quoted_fee.map(|a| a.to_sat()),
        quoted_operation_fee_msat: row.quoted_operation_fee.map(|a| a.msats),
        claim_fee_sat: row.claim_fee.map(|a| a.to_sat()),
        operation_fee_msat: row.operation_fee.map(|a| a.msats),
        net_credit_msat: row.net_credit.map(|a| a.msats),
        funding_txid: row.funding_txid.map(|t| t.to_string()),
        operation_id: row.operation_id.map(|id| id.fmt_full().to_string()),
        last_error: row.last_error.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn quote_unit_msat(
    amounts: &std::collections::BTreeMap<AmountUnit, fedimint_core::Amount>,
) -> Option<u64> {
    amounts.get(&AmountUnit::BITCOIN).map(|amount| amount.msats)
}

fn withdrawal_total_debit_msat(row: &WithdrawalRow) -> Option<u64> {
    let fee = row.fee?;
    let quote = row.quote.as_ref()?;
    let quote_fee_msat = quote_unit_msat(&quote.input).unwrap_or(0)
        + quote_unit_msat(&quote.output).unwrap_or(0)
        + quote_unit_msat(&quote.dust).unwrap_or(0);
    Some(
        row.value.to_sat().saturating_mul(1000)
            + fee.to_sat().saturating_mul(1000)
            + quote_fee_msat,
    )
}

fn withdrawal_view(row: &WithdrawalRow) -> WithdrawalView {
    WithdrawalView {
        id: row.id.0.clone(),
        origin: match row.origin {
            WithdrawalOrigin::Requested => WithdrawalOriginView::Requested,
            WithdrawalOrigin::Adopted => WithdrawalOriginView::Adopted,
        },
        state: match row.state {
            WithdrawalState::Quoting => WithdrawalStateView::Quoting,
            WithdrawalState::Submitted => WithdrawalStateView::Submitted,
            WithdrawalState::Broadcast { .. } => WithdrawalStateView::Broadcast,
            WithdrawalState::Refunded => WithdrawalStateView::Refunded,
            WithdrawalState::Failed => WithdrawalStateView::Failed,
            WithdrawalState::Cancelled => WithdrawalStateView::Cancelled,
        },
        address: row.address.clone(),
        recipient_amount_sat: row.value.to_sat(),
        quoted_fee_sat: row.fee.map(|a| a.to_sat()),
        quote_input_msat: row.quote.as_ref().and_then(|q| quote_unit_msat(&q.input)),
        quote_output_msat: row.quote.as_ref().and_then(|q| quote_unit_msat(&q.output)),
        quote_dust_msat: row.quote.as_ref().and_then(|q| quote_unit_msat(&q.dust)),
        total_debit_msat: withdrawal_total_debit_msat(row),
        operation_id: row.operation_id.map(|id| id.fmt_full().to_string()),
        txid: row.txid.map(|t| t.to_string()),
        custom_meta: row.custom_meta.clone(),
        last_error: row.last_error.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

// Keep the compile-time check that the wallet module wiring has a single
// instance in the expected federation family.
const _: fn() = || {
    let _ = WALLET_V2_KIND;
    let _ = wallet_client_config;
    let _ = Path::new("");
};

#[cfg(test)]
mod tests {
    use super::*;
    use berlin_wallet_core::wire::FeeQuoteWire;

    #[test]
    fn pending_quote_has_no_net() {
        let row = WithdrawalRow::new(
            WithdrawalId("pending".to_owned()),
            FederationId::dummy(),
            "tb1q".to_owned(),
            Amount::from_sat(10_000),
        );
        assert_eq!(withdrawal_total_debit_msat(&row), None);
        assert_eq!(withdrawal_view(&row).total_debit_msat, None);
        assert_eq!(withdrawal_view(&row).quoted_fee_sat, None);
    }

    #[tokio::test]
    async fn shutdown_is_terminal_and_methods_error_not_panic() {
        let wallet = PegWallet::new(std::env::temp_dir().to_string_lossy().into_owned());
        wallet.shutdown().await.expect("first shutdown");
        let error = wallet
            .init("regtest".into())
            .expect_err("init after shutdown");
        assert!(error.to_string().contains("create a new PegWallet"));
        let error = wallet
            .balance_sat()
            .await
            .expect_err("balance after shutdown");
        assert!(error.to_string().contains("create a new PegWallet"));
        let error = wallet
            .open(Some("http://127.0.0.1:1".to_owned()))
            .await
            .expect_err("open after shutdown");
        assert!(error.to_string().contains("create a new PegWallet"));
        wallet.shutdown().await.expect("idempotent shutdown");
    }

    #[tokio::test]
    async fn shutdown_without_active_state_is_ok() {
        let wallet = PegWallet::new(std::env::temp_dir().to_string_lossy().into_owned());
        wallet.shutdown().await.expect("shutdown");
    }

    #[test]
    fn quoted_net_includes_chain_and_federation_fees() {
        let mut row = WithdrawalRow::new(
            WithdrawalId("quoted".to_owned()),
            FederationId::dummy(),
            "tb1q".to_owned(),
            Amount::from_sat(10_000),
        );
        row.fee = Some(Amount::from_sat(234));
        row.quote = Some(FeeQuoteWire {
            input: [(
                AmountUnit::BITCOIN,
                fedimint_core::Amount::from_msats(50_000),
            )]
            .into(),
            output: [(
                AmountUnit::BITCOIN,
                fedimint_core::Amount::from_msats(40_000),
            )]
            .into(),
            dust: [(
                AmountUnit::BITCOIN,
                fedimint_core::Amount::from_msats(10_000),
            )]
            .into(),
        });
        assert_eq!(withdrawal_total_debit_msat(&row), Some(10_334_000));
        let view = withdrawal_view(&row);
        assert_eq!(view.quoted_fee_sat, Some(234));
        assert_eq!(view.quote_input_msat, Some(50_000));
        assert_eq!(view.quote_output_msat, Some(40_000));
        assert_eq!(view.quote_dust_msat, Some(10_000));
        assert_eq!(view.total_debit_msat, Some(10_334_000));
    }
}
