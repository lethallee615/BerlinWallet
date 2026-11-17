//! Sat-first demo console.
//!
//! This is deliberately a demo surface, not a general wallet application. It
//! uses the existing core state machines and formats every amount as `sat`.
//! The normal CLI output and the FFI are not changed.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Context, anyhow};
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet};
use berlin_wallet_core::client::{build_client, client_db, min_withdrawal_sat, open_rocksdb};
use berlin_wallet_core::journal::{Journal, OnchainSendRow};
use berlin_wallet_core::network::{require_recorded_network, verified_client_network};
use berlin_wallet_core::ops::OnchainSendId;
use berlin_wallet_core::sync::{self, DepositWatchers};
use berlin_wallet_core::{deposit, withdrawal};
use bitcoin::{Address, Amount, FeeRate, Network, Txid};
use bitcoincore_rpc::{Auth, Client as BitcoinRpcClient, RpcApi};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use fedimint_client::{ClientHandleArc, RootSecret};
use fedimint_core::config::FederationId;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::AmountUnit;
use fedimint_walletv2_client::WalletClientModule;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

const DEFAULT_SEND_SAT: u64 = 25_000;
const DEFAULT_DEPOSIT_SAT: u64 = 100_000;
const DEFAULT_WITHDRAW_SAT: u64 = 50_000;
const DEFAULT_FEE_RATE: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoProfile {
    Signet,
    Fedimint,
}

impl DemoProfile {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "signet" => Ok(Self::Signet),
            "fedimint" => Ok(Self::Fedimint),
            other => anyhow::bail!("unknown demo profile {other:?}; expected signet|fedimint"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Signet => "signet",
            Self::Fedimint => "fedimint",
        }
    }
}

struct DemoConfig {
    invite: Option<String>,
    recipient: Option<Address>,
    rpc: Option<Arc<BitcoinRpcClient>>,
    send_sat: u64,
    deposit_sat: u64,
    withdraw_sat: u64,
    fee_rate: u64,
}

struct DemoSession {
    profile: DemoProfile,
    network: Network,
    chain_description: String,
    data_dir: PathBuf,
    journal: Journal,
    wallet: Arc<Mutex<BdkWallet>>,
    client: Option<ClientHandleArc>,
    federation_id: Option<FederationId>,
    tasks: Vec<JoinHandle<()>>,
    shutdown_tx: watch::Sender<bool>,
    deposit_watchers: DepositWatchers,
    recipient: Option<Address>,
    rpc: Option<Arc<BitcoinRpcClient>>,
    last_txid: Arc<StdMutex<Option<Txid>>>,
    send_sat: u64,
    deposit_sat: u64,
    withdraw_sat: u64,
    fee_rate: u64,
}

struct Snapshot {
    onchain_sat: u64,
    ecash_sat_msat: u64,
    wallet_address: String,
}

struct RecipientStatus {
    received_sat: u64,
    balance_sat: u64,
    confirmations: Option<u32>,
}

struct SendOutcome {
    address: String,
    amount_sat: u64,
    fee_sat: u64,
    total_debit_sat: u64,
    change_sat: u64,
    txid: Txid,
}

struct DepositOutcome {
    amount_sat: u64,
    state: String,
    ecash_sat_msat: u64,
}

struct WithdrawOutcome {
    address: String,
    amount_sat: u64,
    txid: Option<Txid>,
}

enum UiMsg {
    Snapshot(Snapshot),
    Recipient(RecipientStatus),
    Sent(SendOutcome),
    Deposited(DepositOutcome),
    Withdrawn(WithdrawOutcome),
    Error(String),
}

#[derive(Default)]
struct App {
    profile: String,
    network: String,
    chain: String,
    federation: String,
    onchain_sat: u64,
    ecash_sat_msat: u64,
    wallet_address: String,
    recipient: String,
    recipient_received_sat: Option<u64>,
    recipient_balance_sat: Option<u64>,
    recipient_confirmations: Option<u32>,
    status: String,
    busy: bool,
    log: Vec<String>,
}

impl App {
    fn apply(&mut self, message: UiMsg) {
        self.busy = false;
        match message {
            UiMsg::Snapshot(snapshot) => {
                self.onchain_sat = snapshot.onchain_sat;
                self.ecash_sat_msat = snapshot.ecash_sat_msat;
                self.wallet_address = snapshot.wallet_address;
                self.status = "wallet refreshed".to_owned();
            }
            UiMsg::Recipient(status) => {
                self.recipient_received_sat = Some(status.received_sat);
                self.recipient_balance_sat = Some(status.balance_sat);
                self.recipient_confirmations = status.confirmations;
                self.status = "recipient refreshed".to_owned();
            }
            UiMsg::Sent(outcome) => {
                self.status = format!(
                    "sent {} to {} · fee {} · total debit {} · txid {}",
                    format_sat(outcome.amount_sat),
                    outcome.address,
                    format_sat(outcome.fee_sat),
                    format_sat(outcome.total_debit_sat),
                    outcome.txid
                );
                self.log.push(format!(
                    "send {} · change {} · txid {}",
                    format_sat(outcome.amount_sat),
                    format_sat(outcome.change_sat),
                    outcome.txid
                ));
            }
            UiMsg::Deposited(outcome) => {
                self.ecash_sat_msat = outcome.ecash_sat_msat;
                self.status = format!(
                    "deposit {} · state {} · ecash {}",
                    format_sat(outcome.amount_sat),
                    outcome.state,
                    format_msat_sat(outcome.ecash_sat_msat)
                );
                self.log.push(self.status.clone());
            }
            UiMsg::Withdrawn(outcome) => {
                self.status = format!(
                    "withdraw {} to {}{}",
                    format_sat(outcome.amount_sat),
                    outcome.address,
                    outcome
                        .txid
                        .map(|txid| format!(" · txid {txid}"))
                        .unwrap_or_default()
                );
                self.log.push(self.status.clone());
            }
            UiMsg::Error(error) => {
                self.status = error.clone();
                self.log.push(format!("error: {error}"));
            }
        }
        if self.log.len() > 6 {
            self.log.remove(0);
        }
    }
}

impl DemoSession {
    async fn open(
        profile: DemoProfile,
        config: DemoConfig,
        data_dir: PathBuf,
        seed_bytes: &[u8; 64],
        root_secret: RootSecret,
    ) -> anyhow::Result<Self> {
        let network = require_recorded_network(&data_dir)?;
        let chain = super::chain_config(&data_dir, None)?;
        let chain_description = chain.describe();
        match profile {
            DemoProfile::Signet => {
                let db = open_rocksdb(&data_dir.join("fedimint.rocksdb")).await?;
                let journal = Journal::new(db, vec![0x00]);
                let wallet = BdkWallet::open_or_create(
                    &data_dir.join(BDK_DB_FILE),
                    seed_bytes,
                    network,
                    chain,
                )?;
                let (shutdown_tx, _) = watch::channel(false);
                Ok(Self {
                    profile,
                    network,
                    chain_description,
                    data_dir,
                    journal,
                    wallet: Arc::new(Mutex::new(wallet)),
                    client: None,
                    federation_id: None,
                    tasks: Vec::new(),
                    shutdown_tx,
                    deposit_watchers: DepositWatchers::new(),
                    recipient: config.recipient,
                    rpc: config.rpc,
                    last_txid: Arc::new(StdMutex::new(None)),
                    send_sat: config.send_sat,
                    deposit_sat: config.deposit_sat,
                    withdraw_sat: config.withdraw_sat,
                    fee_rate: config.fee_rate,
                })
            }
            DemoProfile::Fedimint => {
                let invite = config
                    .invite
                    .as_deref()
                    .ok_or_else(|| anyhow!("fedimint demo requires --invite <invite-code>"))?;
                let db = super::open_database(&data_dir).await?;
                let journal = Journal::new(db.clone(), vec![0x00]);
                let connectors = fedimint_connectors().await?;
                let invite = InviteCode::from_str(invite).context("invalid invite code")?;
                let federation_id = invite.federation_id();
                let client =
                    build_client(client_db(&db, 0), connectors, root_secret, invite).await?;
                let chain_network = verified_client_network(&client, network).await?;
                let wallet = BdkWallet::open_or_create(
                    &data_dir.join(BDK_DB_FILE),
                    seed_bytes,
                    chain_network,
                    chain,
                )?;
                let wallet = Arc::new(Mutex::new(wallet));
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let tasks = vec![
                    tokio::spawn(sync::bdk_chain_task(wallet.clone(), shutdown_rx.clone())),
                    tokio::spawn(sync::fedimint_sync_task(
                        client.clone(),
                        journal.clone(),
                        shutdown_rx.clone(),
                    )),
                    tokio::spawn(sync::adoption_task(
                        client.clone(),
                        journal.clone(),
                        federation_id,
                        shutdown_rx.clone(),
                    )),
                ];
                let deposit_watchers = DepositWatchers::new();
                deposit_watchers
                    .resume(&client, &journal, federation_id, shutdown_rx)
                    .await?;
                Ok(Self {
                    profile,
                    network,
                    chain_description,
                    data_dir,
                    journal,
                    wallet,
                    client: Some(client),
                    federation_id: Some(federation_id),
                    tasks,
                    shutdown_tx,
                    deposit_watchers,
                    recipient: config.recipient,
                    rpc: config.rpc,
                    last_txid: Arc::new(StdMutex::new(None)),
                    send_sat: config.send_sat,
                    deposit_sat: config.deposit_sat,
                    withdraw_sat: config.withdraw_sat,
                    fee_rate: config.fee_rate,
                })
            }
        }
    }

    async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let (onchain_sat, wallet_address) = {
            let mut wallet = self.wallet.lock().await;
            let wallet_address = wallet.reveal_destination()?.to_string();
            wallet.sync().await?;
            (wallet.balance().total().to_sat(), wallet_address)
        };
        let ecash_sat_msat = match &self.client {
            Some(client) => {
                client
                    .get_balance_for_unit(AmountUnit::BITCOIN)
                    .await?
                    .msats
            }
            None => 0,
        };
        Ok(Snapshot {
            onchain_sat,
            ecash_sat_msat,
            wallet_address,
        })
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.deposit_watchers.wait_all().await;
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
        if let Some(client) = self.client.take() {
            match Arc::try_unwrap(client) {
                Ok(handle) => handle.shutdown().await,
                Err(_) => eprintln!("warning: demo client still referenced after shutdown"),
            }
        }
        Ok(())
    }
}

async fn fedimint_connectors() -> anyhow::Result<fedimint_connectors::ConnectorRegistry> {
    fedimint_connectors::ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await
}

pub async fn run(
    args: &[String],
    data_dir: &std::path::Path,
    seed_bytes: &[u8; 64],
    root_secret: RootSecret,
    network_arg: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(value) = network_arg {
        let requested = berlin_wallet_core::network::parse_network(value)?;
        anyhow::ensure!(
            requested != Network::Bitcoin,
            "mainnet is disabled in this unaudited alpha"
        );
    }
    let profile_value = arg_value(args, "--profile")
        .or_else(|| std::env::var("BERLIN_WALLET_DEMO_PROFILE").ok())
        .ok_or_else(|| anyhow!("usage: berlin-wallet demo --profile signet|fedimint"))?;
    let profile = DemoProfile::parse(&profile_value)?;
    let invite =
        arg_value(args, "--invite").or_else(|| std::env::var("BERLIN_WALLET_DEMO_INVITE").ok());
    let recipient_raw = arg_value(args, "--recipient")
        .or_else(|| std::env::var("BERLIN_WALLET_DEMO_RECIPIENT").ok());
    let network = require_recorded_network(data_dir)?;
    let recipient = recipient_raw
        .as_deref()
        .map(|value| -> anyhow::Result<Address> {
            let address = Address::from_str(value).context("invalid demo recipient address")?;
            address
                .require_network(network)
                .context("demo recipient address is for a different network")
        })
        .transpose()?;
    let rpc = match std::env::var("BERLIN_WALLET_DEMO_BITCOIND_RPC") {
        Ok(value) => Some(Arc::new(build_rpc_client(&value)?)),
        Err(_) => None,
    };
    let config = DemoConfig {
        invite,
        recipient,
        rpc,
        send_sat: env_u64("BERLIN_WALLET_DEMO_SEND_SAT", DEFAULT_SEND_SAT)?,
        deposit_sat: env_u64("BERLIN_WALLET_DEMO_DEPOSIT_SAT", DEFAULT_DEPOSIT_SAT)?,
        withdraw_sat: env_u64("BERLIN_WALLET_DEMO_WITHDRAW_SAT", DEFAULT_WITHDRAW_SAT)?,
        fee_rate: env_u64("BERLIN_WALLET_DEMO_FEE_RATE", DEFAULT_FEE_RATE)?,
    };
    let mut session = DemoSession::open(
        profile,
        config,
        data_dir.to_owned(),
        seed_bytes,
        root_secret,
    )
    .await?;
    let initial = session.snapshot().await;
    let (onchain_sat, ecash_sat_msat, wallet_address, status) = match initial {
        Ok(snapshot) => (
            snapshot.onchain_sat,
            snapshot.ecash_sat_msat,
            snapshot.wallet_address,
            "ready — press 1 to refresh".to_owned(),
        ),
        Err(error) => (0, 0, "<unavailable>".to_owned(), format!("{error:#}")),
    };
    let app = App {
        profile: session.profile.label().to_owned(),
        network: session.network.to_string(),
        chain: session.chain_description.clone(),
        federation: session
            .federation_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        recipient: session
            .recipient
            .clone()
            .map(|address| address.to_string())
            .unwrap_or_else(|| "not configured".to_owned()),
        onchain_sat,
        ecash_sat_msat,
        wallet_address,
        status,
        ..App::default()
    };
    let result = run_tui(&mut session, app).await;
    let shutdown = session.shutdown().await;
    ratatui::try_restore().ok();
    println!(
        "demo shutdown complete; data dir: {}",
        session.data_dir.display()
    );
    println!("safe to back up or restart the wallet");
    result?;
    shutdown
}

async fn run_tui(session: &mut DemoSession, mut app: App) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();
    let running = Arc::new(AtomicBool::new(true));
    let input_running = running.clone();
    std::thread::spawn(move || {
        while input_running.load(Ordering::SeqCst) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if key_tx.send(key).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<UiMsg>();
    let mut action: Option<JoinHandle<()>> = None;
    let result = loop {
        terminal.draw(|frame| render(frame, &app))?;
        tokio::select! {
            Some(key) = key_rx.recv() => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break Ok(());
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break Ok(()),
                    KeyCode::Char('1') => {
                        if !app.busy {
                            app.busy = true;
                            app.status = "refreshing wallet".to_owned();
                            action = Some(spawn_refresh(session, update_tx.clone()));
                        }
                    }
                    KeyCode::Char('2') => {
                        if !app.busy {
                            app.busy = true;
                            app.status = "sending to recipient".to_owned();
                            action = Some(spawn_send(session, update_tx.clone()));
                        }
                    }
                    KeyCode::Char('3') => {
                        if !app.busy {
                            app.busy = true;
                            app.status = "refreshing recipient".to_owned();
                            action = Some(spawn_recipient(session, update_tx.clone()));
                        }
                    }
                    KeyCode::Char('4') => {
                        if !app.busy && session.client.is_some() {
                            app.busy = true;
                            app.status = "deposit in progress".to_owned();
                            action = Some(spawn_deposit(session, update_tx.clone()));
                        }
                    }
                    KeyCode::Char('5') => {
                        if !app.busy && session.client.is_some() {
                            app.busy = true;
                            app.status = "withdrawal in progress".to_owned();
                            action = Some(spawn_withdraw(session, update_tx.clone()));
                        }
                    }
                    _ => {}
                }
            }
            Some(message) = update_rx.recv() => {
                app.apply(message);
                action = None;
            }
        }
    };
    running.store(false, Ordering::SeqCst);
    if let Some(action) = action {
        action.abort();
    }
    ratatui::try_restore()?;
    result
}

fn spawn_refresh(session: &DemoSession, tx: mpsc::UnboundedSender<UiMsg>) -> JoinHandle<()> {
    let wallet = session.wallet.clone();
    let client = session.client.clone();
    tokio::spawn(async move {
        let result = async {
            let (onchain_sat, wallet_address) = {
                let mut wallet = wallet.lock().await;
                wallet.sync().await?;
                (
                    wallet.balance().total().to_sat(),
                    wallet.reveal_destination()?.to_string(),
                )
            };
            let ecash_sat_msat = match client {
                Some(client) => {
                    client
                        .get_balance_for_unit(AmountUnit::BITCOIN)
                        .await?
                        .msats
                }
                None => 0,
            };
            Ok::<Snapshot, anyhow::Error>(Snapshot {
                onchain_sat,
                ecash_sat_msat,
                wallet_address,
            })
        }
        .await;
        match result {
            Ok(snapshot) => {
                let _ = tx.send(UiMsg::Snapshot(snapshot));
            }
            Err(error) => {
                let _ = tx.send(UiMsg::Error(format!("{error:#}")));
            }
        }
    })
}

fn spawn_recipient(session: &DemoSession, tx: mpsc::UnboundedSender<UiMsg>) -> JoinHandle<()> {
    let session = RecipientProbe {
        recipient: session.recipient.clone(),
        rpc: session.rpc.clone(),
        last_txid: session.last_txid.clone(),
    };
    tokio::spawn(async move {
        match session.status().await {
            Ok(status) => {
                let _ = tx.send(UiMsg::Recipient(status));
            }
            Err(error) => {
                let _ = tx.send(UiMsg::Error(format!("{error:#}")));
            }
        }
    })
}

struct RecipientProbe {
    recipient: Option<Address>,
    rpc: Option<Arc<BitcoinRpcClient>>,
    last_txid: Arc<StdMutex<Option<Txid>>>,
}

impl RecipientProbe {
    async fn status(&self) -> anyhow::Result<RecipientStatus> {
        let address = self
            .recipient
            .clone()
            .ok_or_else(|| anyhow!("BERLIN_WALLET_DEMO_RECIPIENT is not configured"))?;
        let rpc = self
            .rpc
            .as_ref()
            .ok_or_else(|| anyhow!("BERLIN_WALLET_DEMO_BITCOIND_RPC is not configured"))?;
        let received_sat = rpc.get_received_by_address(&address, Some(0))?.to_sat();
        let balance_sat = rpc.get_balance(None, None)?.to_sat();
        let confirmations = self
            .last_txid
            .lock()
            .ok()
            .and_then(|txid| *txid)
            .and_then(|txid| rpc.get_transaction(&txid, None).ok())
            .map(|result| result.info.confirmations.max(0) as u32);
        Ok(RecipientStatus {
            received_sat,
            balance_sat,
            confirmations,
        })
    }
}

fn spawn_send(session: &DemoSession, tx: mpsc::UnboundedSender<UiMsg>) -> JoinHandle<()> {
    let wallet = session.wallet.clone();
    let journal = session.journal.clone();
    let recipient = session.recipient.clone();
    let last_txid = session.last_txid.clone();
    let send_sat = session.send_sat;
    let fee_rate_sat_vb = session.fee_rate;
    tokio::spawn(async move {
        let result = async {
            let address = recipient
                .ok_or_else(|| anyhow!("BERLIN_WALLET_DEMO_RECIPIENT is not configured"))?;
            let fee_rate = FeeRate::from_sat_per_vb(fee_rate_sat_vb)
                .ok_or_else(|| anyhow!("fee rate is too high"))?;
            let amount = Amount::from_sat(send_sat);
            let mut wallet = wallet.lock().await;
            wallet.sync().await?;
            let plan = wallet.quote_send(&address, amount, fee_rate)?;
            let amount_sat = plan.quote.amount.to_sat();
            let fee_sat = plan.quote.fee.to_sat();
            let total_debit_sat = plan.quote.total_debit.to_sat();
            let change_sat = plan.quote.change.to_sat();
            let txid = plan.quote.txid;
            let signed = wallet.execute_send(plan).await?;
            let row = OnchainSendRow::broadcast(
                OnchainSendId::new(),
                address.to_string(),
                amount,
                fee_rate_sat_vb,
                signed.quote.fee,
                signed.vsize,
                signed.quote.change,
                signed.txid,
            );
            journal.insert_onchain_send(&row).await?;
            if let Ok(mut last) = last_txid.lock() {
                *last = Some(signed.txid);
            }
            Ok::<SendOutcome, anyhow::Error>(SendOutcome {
                address: address.to_string(),
                amount_sat,
                fee_sat,
                total_debit_sat,
                change_sat,
                txid,
            })
        }
        .await;
        match result {
            Ok(outcome) => {
                let _ = tx.send(UiMsg::Sent(outcome));
            }
            Err(error) => {
                let _ = tx.send(UiMsg::Error(format!("{error:#}")));
            }
        }
    })
}

fn spawn_deposit(session: &DemoSession, tx: mpsc::UnboundedSender<UiMsg>) -> JoinHandle<()> {
    let client = session.client.clone();
    let journal = session.journal.clone();
    let wallet = session.wallet.clone();
    let federation_id = session.federation_id;
    let deposit_sat = session.deposit_sat;
    tokio::spawn(async move {
        let result = async {
            let client = client.ok_or_else(|| anyhow!("deposit requires the fedimint profile"))?;
            let federation_id =
                federation_id.ok_or_else(|| anyhow!("demo session has no federation id"))?;
            let amount = Amount::from_sat(deposit_sat);
            let row = deposit::request_deposit(&client, &journal, federation_id, amount).await?;
            let module = client.get_first_module::<WalletClientModule>()?;
            deposit::run_address_task(&journal, module.inner(), &row.id).await?;
            let row = journal
                .deposit(&row.id)
                .await?
                .ok_or_else(|| anyhow!("deposit row disappeared"))?;
            if row.address.is_none() {
                anyhow::bail!("deposit address is not ready");
            }
            {
                let mut wallet = wallet.lock().await;
                deposit::fund_deposit(&journal, module.inner(), &mut wallet, &row.id).await?;
            }
            let position = row
                .position
                .ok_or_else(|| anyhow!("deposit has no event-log position"))?;
            deposit::watch_deposit(
                &client,
                &journal,
                module.inner(),
                &row.id,
                federation_id,
                position,
            )
            .await?;
            let row = journal
                .deposit(&row.id)
                .await?
                .ok_or_else(|| anyhow!("deposit row disappeared after watch"))?;
            let ecash_sat_msat = client
                .get_balance_for_unit(AmountUnit::BITCOIN)
                .await?
                .msats;
            Ok::<DepositOutcome, anyhow::Error>(DepositOutcome {
                amount_sat: row.amount.to_sat(),
                state: format!("{:?}", row.state),
                ecash_sat_msat,
            })
        }
        .await;
        match result {
            Ok(outcome) => {
                let _ = tx.send(UiMsg::Deposited(outcome));
            }
            Err(error) => {
                let _ = tx.send(UiMsg::Error(format!("{error:#}")));
            }
        }
    })
}

fn spawn_withdraw(session: &DemoSession, tx: mpsc::UnboundedSender<UiMsg>) -> JoinHandle<()> {
    let client = session.client.clone();
    let journal = session.journal.clone();
    let federation_id = session.federation_id;
    let recipient = session.recipient.clone();
    let last_txid = session.last_txid.clone();
    let withdraw_sat = session.withdraw_sat;
    tokio::spawn(async move {
        let result = async {
            let client =
                client.ok_or_else(|| anyhow!("withdrawal requires the fedimint profile"))?;
            let federation_id =
                federation_id.ok_or_else(|| anyhow!("demo session has no federation id"))?;
            let address = recipient
                .ok_or_else(|| anyhow!("BERLIN_WALLET_DEMO_RECIPIENT is not configured"))?;
            let min = min_withdrawal_sat(&client).await?;
            anyhow::ensure!(
                withdraw_sat >= min,
                "withdrawal {} sat is below the federation minimum {} sat",
                withdraw_sat,
                min
            );
            let row = withdrawal::request_withdrawal(
                &client,
                &journal,
                federation_id,
                address.to_string(),
                Amount::from_sat(withdraw_sat),
            )
            .await?;
            withdrawal::send_withdrawal(&client, &journal, &row.id).await?;
            let row = journal
                .withdrawal(&row.id)
                .await?
                .ok_or_else(|| anyhow!("withdrawal row disappeared"))?;
            if let Some(txid) = row.txid
                && let Ok(mut last) = last_txid.lock()
            {
                *last = Some(txid);
            }
            Ok::<WithdrawOutcome, anyhow::Error>(WithdrawOutcome {
                address: address.to_string(),
                amount_sat: row.value.to_sat(),
                txid: row.txid,
            })
        }
        .await;
        match result {
            Ok(outcome) => {
                let _ = tx.send(UiMsg::Withdrawn(outcome));
            }
            Err(error) => {
                let _ = tx.send(UiMsg::Error(format!("{error:#}")));
            }
        }
    })
}

fn build_rpc_client(raw: &str) -> anyhow::Result<BitcoinRpcClient> {
    let (url, auth) = parse_rpc_url(raw)?;
    Ok(BitcoinRpcClient::new(&url, auth)?)
}

fn parse_rpc_url(raw: &str) -> anyhow::Result<(String, Auth)> {
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| anyhow!("RPC URL must start with http:// or https://"))?;
    anyhow::ensure!(
        scheme == "http" || scheme == "https",
        "RPC URL must start with http:// or https://"
    );
    let (auth, host) = match rest.rsplit_once('@') {
        Some((auth, host)) => (Some(auth), host),
        None => (None, rest),
    };
    let auth = match auth {
        Some(value) => {
            let (user, password) = value
                .split_once(':')
                .ok_or_else(|| anyhow!("RPC credentials must be user:password"))?;
            Auth::UserPass(user.to_owned(), password.to_owned())
        }
        None => Auth::None,
    };
    Ok((format!("{scheme}://{host}"), auth))
}

fn env_u64(name: &str, default: u64) -> anyhow::Result<u64> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be an integer")),
        Err(_) => Ok(default),
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|position| args.get(position + 1))
        .cloned()
}

fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .split(frame.area());

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Berlin Wallet demo",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(app.profile.clone(), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(format!("Network: {}   Chain: {}", app.network, app.chain)),
        Line::from(format!("Federation: {}", app.federation)),
        Line::from(format!("On-chain: {}", format_sat(app.onchain_sat))),
        Line::from(format!("Ecash: {}", format_msat_sat(app.ecash_sat_msat))),
        Line::from(format!("Wallet address: {}", app.wallet_address)),
        Line::from(format!("Recipient: {}", app.recipient)),
        Line::from(
            match (app.recipient_received_sat, app.recipient_balance_sat) {
                (Some(received), Some(balance)) => format!(
                    "Received: {}   Balance: {}{}",
                    format_sat(received),
                    format_sat(balance),
                    app.recipient_confirmations
                        .map(|value| format!("   Confirmations: {value}"))
                        .unwrap_or_default()
                ),
                _ => "Received: press 3 to query".to_owned(),
            },
        ),
    ])
    .block(Block::default().borders(Borders::ALL).title("wallet"));
    frame.render_widget(header, chunks[0]);

    let mut items = vec![
        ListItem::new("[1] Show wallet and network"),
        ListItem::new("[2] Send to external recipient"),
        ListItem::new("[3] Confirm / refresh + recipient balance"),
    ];
    if app.federation == "none" {
        items.push(ListItem::new("[4] Deposit (fedimint profile only)"));
        items.push(ListItem::new("[5] Withdraw (fedimint profile only)"));
    } else {
        items.push(ListItem::new("[4] Deposit to ecash"));
        items.push(ListItem::new("[5] Withdraw to external recipient"));
    }
    items.push(ListItem::new("[q] Quit (clean shutdown)"));
    let menu = List::new(items).block(Block::default().borders(Borders::ALL).title("tasks"));
    frame.render_widget(menu, chunks[1]);

    let status = Paragraph::new(format!(
        "{}{}",
        if app.busy { "working: " } else { "" },
        app.status
    ))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("status"));
    frame.render_widget(status, chunks[2]);

    let log = Paragraph::new(app.log.join("\n"))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("log"));
    frame.render_widget(log, chunks[3]);
}

fn format_sat(sat: u64) -> String {
    format!("{} sat", group_digits(sat))
}

fn format_msat_sat(msat: u64) -> String {
    let sat = msat / 1_000;
    let millis = msat % 1_000;
    if millis == 0 {
        format_sat(sat)
    } else {
        format!("{}.{:03} sat", group_digits(sat), millis)
    }
}

fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sat_formatting_uses_commas_and_no_other_units() {
        assert_eq!(format_sat(0), "0 sat");
        assert_eq!(format_sat(999), "999 sat");
        assert_eq!(format_sat(1_000), "1,000 sat");
        assert_eq!(format_sat(1_234_567), "1,234,567 sat");
    }

    #[test]
    fn msat_formatting_is_sat_with_fractional_precision() {
        assert_eq!(format_msat_sat(0), "0 sat");
        assert_eq!(format_msat_sat(1_000), "1 sat");
        assert_eq!(format_msat_sat(1_100), "1.100 sat");
        assert_eq!(format_msat_sat(99_577_856), "99,577.856 sat");
    }

    #[test]
    fn ui_strings_do_not_use_btc_or_msat_labels() {
        let rendered = format!(
            "{} {} {}",
            format_sat(1),
            format_msat_sat(99_577_856),
            "Recipient balance"
        );
        assert!(!rendered.contains("BTC"));
        assert!(!rendered.contains("msat"));
    }

    #[test]
    fn rpc_url_accepts_user_pass_and_wallet_path() {
        let (url, auth) = parse_rpc_url("http://u:p@127.0.0.1:18443/wallet/recipient").unwrap();
        assert_eq!(url, "http://127.0.0.1:18443/wallet/recipient");
        assert!(matches!(auth, Auth::UserPass(user, pass) if user == "u" && pass == "p"));
    }
}
