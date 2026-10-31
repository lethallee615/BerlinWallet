use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use berlin_wallet_core::backup::{backup_data_dir, import_data_dir};
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet};
use berlin_wallet_core::chain::ChainBackendConfig;
use berlin_wallet_core::client::{
    build_client, client_db, min_withdrawal_sat, open_rocksdb, recover_client,
};
use berlin_wallet_core::journal::{
    DepositRow, DepositState, Journal, LightningRow, OnchainSendRow, collapse_deposit_rows,
};
use berlin_wallet_core::lightning;
use berlin_wallet_core::network::{
    NETWORK_FILE, init_network, network_from_chain_id, network_from_recorded, parse_network,
    require_recorded_network, verified_client_network,
};
use berlin_wallet_core::ops::{DepositId, LightningId, OnchainSendId, WithdrawalId};
use berlin_wallet_core::restore_policy::RestoreSummary;
use berlin_wallet_core::seed;
use berlin_wallet_core::sync;
use berlin_wallet_core::withdrawal::await_change_outputs;
use berlin_wallet_core::{deposit, withdrawal};
use bitcoin::{Amount, Network};
use fedimint_client::RootSecret;
use fedimint_client::{Client, ClientHandleArc};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::core::OperationId;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::AmountUnit;
use fedimint_walletv2_client::WalletClientModule;
use tokio::sync::{Mutex, watch};

const DEFAULT_ESPLORA_URL: &str = "http://127.0.0.1:3002";
const WATCH_HEARTBEAT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let mut args: Vec<String> = std::env::args().collect();
    let network_arg = take_network_flag(&mut args)?;
    let data_dir = data_dir();
    let command = args.get(1).map(String::as_str);

    // Directory backup/restore must not open the client DB or create a mnemonic.
    if command == Some("backup") {
        let destination = args
            .get(2)
            .ok_or_else(|| anyhow!("usage: berlin-wallet backup <destination-file>"))?;
        let passphrase = new_passphrase("backup")?;
        return backup_data_dir(&data_dir, Path::new(destination), &passphrase);
    }
    if command == Some("restore-backup") {
        let source = args
            .get(2)
            .ok_or_else(|| anyhow!("usage: berlin-wallet restore-backup <source-file>"))?;
        let passphrase = existing_passphrase("backup")?;
        return import_data_dir(&data_dir, Path::new(source), &passphrase);
    }

    std::fs::create_dir_all(&data_dir)?;
    let mnemonic_path = data_dir.join("mnemonic");
    if command == Some("restore") && !mnemonic_path.exists() {
        anyhow::bail!(
            "restore requires an existing mnemonic at {}",
            mnemonic_path.display()
        );
    }
    let passphrase = if command == Some("init")
        || !mnemonic_path.exists()
        || std::fs::read(&mnemonic_path)
            .map(|bytes| !seed::is_encrypted_mnemonic(&bytes))
            .unwrap_or(false)
    {
        new_passphrase("wallet")?
    } else {
        existing_passphrase("wallet")?
    };
    let mnemonic = seed::load_or_create_mnemonic(&mnemonic_path, &passphrase)?;
    let seed = seed::mnemonic_seed(&mnemonic);
    let root_secret = seed::root_secret_from_mnemonic(&mnemonic);
    match command {
        None | Some("init") => {
            let value = network_arg.as_deref().ok_or_else(|| {
                anyhow!("init requires --network <mainnet|testnet|signet|regtest>")
            })?;
            let network = init_network(&data_dir, parse_network(value)?)?;
            // `init` takes the chain source as its first positional argument.
            let chain = chain_config(&data_dir, args.get(2).cloned())?;
            let mut bdk = BdkWallet::open_or_create(
                &data_dir.join(BDK_DB_FILE),
                &seed,
                network,
                chain.clone(),
            )?;
            println!("chain source: {}", chain.describe());
            println!(
                "network: {network} (recorded in {})",
                data_dir.join(NETWORK_FILE).display()
            );
            println!("data dir: {}", data_dir.display());
            println!("mnemonic: {} (encrypted)", mnemonic_path.display());
            println!("bdk db: {}", data_dir.join(BDK_DB_FILE).display());
            println!(
                "fedimint db: {}",
                data_dir.join("fedimint.rocksdb").display()
            );
            println!("bdk address: {}", bdk.reveal_destination()?);
            println!("bdk balance: {}", bdk.balance());
            print_help();
            Ok(())
        }
        Some("join") => {
            let invite = args
                .get(2)
                .ok_or_else(|| anyhow!("usage: berlin-wallet join <invite-code>"))?;
            join(invite, root_secret, &data_dir, network_arg.as_deref()).await
        }
        Some("run") => {
            let invite = args
                .get(2)
                .ok_or_else(|| anyhow!("usage: berlin-wallet run <invite-code> [chain-source]"))?;
            let recorded = require_recorded_network(&data_dir)?;
            if let Some(value) = network_arg.as_deref() {
                let requested = parse_network(value)?;
                anyhow::ensure!(
                    requested == recorded,
                    "run uses the network recorded in the data directory ({recorded}); \
                     --network {requested} conflicts. Use a fresh data directory to change it"
                );
            }
            run(
                invite,
                &seed,
                root_secret,
                &data_dir,
                recorded,
                args.get(3).cloned(),
            )
            .await
        }
        Some("restore") => {
            let invite = args.get(2).ok_or_else(|| {
                anyhow!(
                    "usage: berlin-wallet restore --network <net> <invite-code> [chain-source] [expected-fed-id] [expected-client-identity]"
                )
            })?;
            let value = network_arg.as_deref().ok_or_else(|| {
                anyhow!("restore requires --network <mainnet|testnet|signet|regtest>")
            })?;
            let recorded = parse_network(value)?;
            restore(
                invite,
                &seed,
                root_secret,
                &data_dir,
                recorded,
                RestoreOptions {
                    chain_source: args.get(3).cloned(),
                    expected_federation_id: args.get(4).map(String::as_str),
                    expected_identity: args.get(5).map(String::as_str),
                },
            )
            .await
        }
        Some(other) => {
            print_help();
            anyhow::bail!("unknown command: {other}")
        }
    }
}

async fn open_database(data_dir: &Path) -> anyhow::Result<Database> {
    open_rocksdb(&data_dir.join("fedimint.rocksdb")).await
}

fn passphrase_env(kind: &str) -> &'static str {
    match kind {
        "backup" => "BERLIN_WALLET_BACKUP_PASSPHRASE",
        _ => "BERLIN_WALLET_PASSPHRASE",
    }
}

fn checked_passphrase(value: String, kind: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!value.is_empty(), "{kind} passphrase must not be empty");
    Ok(value)
}

/// Read a passphrase for an existing wallet or backup, from the environment in
/// automation or from the terminal otherwise.
fn existing_passphrase(kind: &str) -> anyhow::Result<String> {
    if let Ok(value) = std::env::var(passphrase_env(kind)) {
        return checked_passphrase(value, kind);
    }
    checked_passphrase(
        rpassword::prompt_password(format!("{kind} passphrase: "))?,
        kind,
    )
}

/// Read and confirm a new wallet or backup passphrase. The environment form is
/// intended for automation and does not prompt a second time.
fn new_passphrase(kind: &str) -> anyhow::Result<String> {
    if let Ok(value) = std::env::var(passphrase_env(kind)) {
        return checked_passphrase(value, kind);
    }
    let first = checked_passphrase(
        rpassword::prompt_password(format!("New {kind} passphrase: "))?,
        kind,
    )?;
    let second = checked_passphrase(
        rpassword::prompt_password(format!("Confirm {kind} passphrase: "))?,
        kind,
    )?;
    anyhow::ensure!(first == second, "{kind} passphrases do not match");
    Ok(first)
}

/// Reloadable stderr tracing filter. `RUST_LOG` controls filtering and defaults
/// to `info`; the handle lets shutdown silence fedimint's task-cancellation
/// lifecycle noise without hiding real task errors during normal operation.
type TracingReload =
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;
static TRACING_RELOAD: OnceLock<TracingReload> = OnceLock::new();

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(filter);
    let subscriber = tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    if subscriber.try_init().is_ok() {
        let _ = TRACING_RELOAD.set(reload_handle);
    }
}

/// Silence fedimint's task-cancellation lifecycle logs for the shutdown phase.
///
/// `ClientHandle::shutdown` cancels the client task group; the walletv2 output
/// scanner reports that cancellation as an ERROR even though it is expected.
/// Only `fm::task` is disabled, and only after the REPL has stopped taking
/// commands, so genuine task panics during normal operation stay visible.
fn quiet_fedimint_task_logs() {
    let Some(handle) = TRACING_RELOAD.get() else {
        return;
    };
    let Ok(directive) = "fm::task=off".parse::<tracing_subscriber::filter::Directive>() else {
        return;
    };
    let _ = handle.modify(|filter| {
        *filter = filter.clone().add_directive(directive);
    });
}

/// Resolve the chain source for `data_dir`.
///
/// Precedence: the argument, then `BERLIN_WALLET_CHAIN`, then the value recorded
/// in `chain.json`. If nothing is configured, fall back to the Esplora default
/// and record it, so a second backend can never be switched in silently.
fn chain_config(data_dir: &Path, requested: Option<String>) -> anyhow::Result<ChainBackendConfig> {
    let requested = requested
        .or_else(|| std::env::var("BERLIN_WALLET_CHAIN").ok())
        .filter(|source| !source.trim().is_empty());
    if let Some(config) =
        berlin_wallet_core::chain::persist::resolve_chain_config(data_dir, requested.as_deref())?
    {
        return Ok(config);
    }
    let default: ChainBackendConfig = DEFAULT_ESPLORA_URL
        .parse()
        .expect("the default Esplora URL always parses");
    berlin_wallet_core::chain::persist::store_chain_config(data_dir, &default)?;
    Ok(default)
}

fn data_dir() -> PathBuf {
    std::env::var_os("BERLIN_WALLET_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".berlin-wallet"))
}

fn print_help() {
    println!();
    println!("commands:");
    println!("  init --network <net> [chain-source] create/open the local wallet; net is");
    println!("                                      mainnet|testnet|signet|regtest");
    println!("  join <invite> [--network <net>]     join a federation and print its config");
    println!(
        "  backup <dest-file>                   encrypted age archive of mnemonic + client DB + BDK DB"
    );
    println!("  restore-backup <src-file>            decrypt an archive into an empty data dir");
    println!("  restore --network <net> <invite> [chain-source] [fed] [identity]");
    println!();
    println!("  Wallet and backup passphrases are prompted; in automation set");
    println!("  BERLIN_WALLET_PASSPHRASE and BERLIN_WALLET_BACKUP_PASSPHRASE.");
    println!("                                      recover on a fresh data dir + full BDK scan");
    println!("  run <invite> [chain-source]         interactive manual-run session");
    println!();
    println!("  An unknown federation chain-id fails closed unless the data directory");
    println!("  was explicitly initialized with --network regtest (devimint/custom).");
    println!();
    println!("interactive commands:");
    println!("  balance                             BDK balance");
    println!("  ecash-balance                       federation BTC ecash balance");
    println!("  address                             reveal the next BDK address");
    println!("  min                                 federation minimum withdrawal (sat)");
    println!("  request-deposit <sat>               request a deposit address");
    println!("  fund-deposit <deposit-id>           fund an address-ready deposit");
    println!("  deposit <sat>                       request + fund in one step");
    println!(
        "  watch <deposit-id>                  await the deposit's claim (progress on stderr)"
    );
    println!("  request-withdraw <address> <sat>    create a withdrawal row");
    println!("  send-withdraw <withdrawal-id>       quote + send an existing row");
    println!("  cancel-deposit <deposit-id>         cancel an open deposit row");
    println!("  cancel-withdraw <withdrawal-id>     cancel a Quoting row");
    println!("  list-withdrawals                    list withdrawal rows");
    println!("  list-deposits                       list deposit rows (including adopted)");
    println!("  operations                          print recent operation-log entries");
    println!("  operation-fees <operation-id>       show operation fees");
    println!("  await-change <operation-id>         await primary-module ecash change outputs");
    println!("  withdraw <address> <sat>            request + send in one step");
    println!("  send <address> <sat> <sat/vb>       send on-chain from the BDK wallet");
    println!("  list-onchain                        journalled sends + wallet history");
    println!("  ln-send <invoice> [gateway]         pay a Bolt11 invoice via lnv2");
    println!("  ln-receive <sat> [expiry] [gateway] [description...]");
    println!("                                      create an lnv2 invoice");
    println!("  ln-status <id>                      print one lightning row");
    println!("  ln-list                             list lightning rows");
    println!("  ln-gateways                         list the federation's vetted gateways");
    println!("  status <id>                         print a deposit or withdrawal row");
    println!("  sync                                run one BDK Esplora sync");
    println!("  help");
    println!("  quit");
    println!();
    println!("  Ctrl+C cancels a running `watch`; use `quit` to exit.");
    println!("  Ctrl+Z is refused while the wallet holds the database lock.");
}

async fn join(
    invite: &str,
    root_secret: RootSecret,
    data_dir: &Path,
    requested_network: Option<&str>,
) -> anyhow::Result<()> {
    let db = open_database(data_dir).await?;
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let identity = seed::client_identity(&root_secret, invite.federation_id())?;
    let client = build_client(client_db(&db, 0), connectors, root_secret, invite).await?;

    let module = client.get_first_module::<WalletClientModule>()?;
    let chain_id = client.chain_id().await?;
    let chain_network = match requested_network {
        Some(value) => network_from_recorded(chain_id, parse_network(value)?)?,
        None => network_from_chain_id(chain_id)?,
    };
    println!("federation id: {}", client.federation_id());
    println!("client identity: {identity}");
    println!("chain-id network: {chain_network}");
    println!("config network: {}", module.get_network());
    for (id, config) in &client.config().await.modules {
        println!("module {id}: {}", config.kind);
    }
    println!(
        "primary BTC module: {:?}",
        client
            .primary_module_for_unit(AmountUnit::BITCOIN)
            .map(|(id, _)| id)
    );
    println!("min withdrawal: {} sat", min_withdrawal_sat(&client).await?);
    Ok(())
}

async fn run(
    invite: &str,
    seed: &[u8; 64],
    root_secret: RootSecret,
    data_dir: &Path,
    recorded: Network,
    requested_chain: Option<String>,
) -> anyhow::Result<()> {
    let db = open_database(data_dir).await?;
    let journal = Journal::new(db.clone(), vec![0x00]);
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let identity = seed::client_identity(&root_secret, invite.federation_id())?;
    let client = build_client(client_db(&db, 0), connectors, root_secret, invite).await?;
    let federation_id = client.federation_id();
    let chain_network = verified_client_network(&client, recorded).await?;
    let chain = chain_config(data_dir, requested_chain)?;
    let bdk = BdkWallet::open_or_create(
        &data_dir.join(BDK_DB_FILE),
        seed,
        chain_network,
        chain.clone(),
    )?;
    let wallet = Arc::new(Mutex::new(bdk));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = Vec::new();
    tasks.push(tokio::spawn(sync::bdk_chain_task(
        wallet.clone(),
        shutdown_rx.clone(),
    )));
    tasks.push(tokio::spawn(sync::fedimint_sync_task(
        client.clone(),
        journal.clone(),
        shutdown_rx.clone(),
    )));
    tasks.push(tokio::spawn(sync::adoption_task(
        client.clone(),
        journal.clone(),
        federation_id,
        shutdown_rx.clone(),
    )));

    // SIGINT cancels the current command instead of killing the process;
    // SIGTSTP is trapped so Ctrl+Z cannot leave the database lock held.
    let (interrupt_tx, interrupt_rx) = watch::channel(0u64);
    tasks.push(spawn_interrupt_listener(interrupt_tx, shutdown_rx.clone()));
    #[cfg(unix)]
    tasks.push(spawn_suspend_guard(shutdown_rx.clone()));

    let deposit_watchers = sync::DepositWatchers::new();
    let resumed = deposit_watchers
        .resume(&client, &journal, federation_id, shutdown_rx.clone())
        .await?;
    if resumed > 0 {
        println!("resumed {resumed} funded deposit watcher(s)");
    }

    let module = client.get_first_module::<WalletClientModule>()?;
    println!("federation id: {federation_id}");
    println!("client identity: {identity}");
    println!("recorded network: {recorded}");
    println!("chain-id network: {chain_network}");
    println!("config network: {}", module.get_network());
    println!("chain source: {}", chain.describe());
    println!("bdk address: {}", wallet.lock().await.reveal_destination()?);
    println!("min withdrawal: {} sat", min_withdrawal_sat(&client).await?);
    println!("type `help` for commands");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        eprint!("berlin-wallet> ");
        io::stderr().flush()?;
        let Some(line) = lines.next() else { break };
        let line = line?;
        let mut parts = line.split_whitespace();
        let Some(command) = parts.next() else {
            continue;
        };

        let result = match command {
            "quit" | "exit" => {
                let _ = shutdown_tx.send(true);
                break;
            }
            "help" => {
                print_help();
                Ok(())
            }
            "balance" => {
                println!("{}", wallet.lock().await.balance());
                Ok(())
            }
            "ecash-balance" => {
                let balance = client.get_balance_for_unit(AmountUnit::BITCOIN).await?;
                println!("{} msat ({} sat)", balance.msats, balance.msats / 1000);
                Ok(())
            }
            "address" => {
                println!("{}", wallet.lock().await.reveal_destination()?);
                Ok(())
            }
            "min" => {
                println!("{} sat", min_withdrawal_sat(&client).await?);
                Ok(())
            }
            "sync" => match wallet.lock().await.sync().await {
                Ok(()) => {
                    println!("bdk synced");
                    Ok(())
                }
                Err(err) => Err(err),
            },
            "request-deposit" => {
                let amount = parse_sat(parts.next())?;
                command_request_deposit(&client, &journal, federation_id, amount).await
            }
            "fund-deposit" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: fund-deposit <deposit-id>"))?;
                command_fund_deposit(
                    &client,
                    &journal,
                    &wallet,
                    federation_id,
                    &deposit_watchers,
                    shutdown_tx.subscribe(),
                    &DepositId(id.to_owned()),
                )
                .await
            }
            "deposit" => {
                let amount = parse_sat(parts.next())?;
                command_deposit(
                    &client,
                    &journal,
                    &wallet,
                    federation_id,
                    &deposit_watchers,
                    shutdown_tx.subscribe(),
                    amount,
                )
                .await
            }
            "watch" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: watch <deposit-id>"))?;
                let deposit_id = DepositId(id.to_owned());
                let mut interrupt = interrupt_rx.clone();
                tokio::select! {
                    result = command_watch(
                        &client,
                        &journal,
                        federation_id,
                        &deposit_id,
                    ) => result,
                    _ = interrupt.changed() => {
                        eprintln!(
                            "watch cancelled; the background observer (or the 60 s adoption pass) keeps the row updated"
                        );
                        Ok(())
                    }
                }
            }
            "request-withdraw" => {
                let address = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: request-withdraw <address> <sat>"))?;
                let amount = parse_sat(parts.next())?;
                command_request_withdraw(&client, &journal, federation_id, address, amount).await
            }
            "send-withdraw" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: send-withdraw <withdrawal-id>"))?;
                command_send_withdraw(&client, &journal, &WithdrawalId(id.to_owned())).await
            }
            "cancel-deposit" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: cancel-deposit <deposit-id>"))?;
                let result = journal.cancel_deposit(&DepositId(id.to_owned())).await?;
                println!("cancel result: {result:?}");
                Ok(())
            }
            "cancel-withdraw" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: cancel-withdraw <withdrawal-id>"))?;
                let result = journal
                    .cancel_withdrawal(&WithdrawalId(id.to_owned()))
                    .await?;
                println!("cancel result: {result:?}");
                Ok(())
            }
            "operations" => {
                for (key, entry) in client
                    .operation_log()
                    .paginate_operations_rev(20, None)
                    .await
                {
                    println!(
                        "operation_id_full={} {key:?} {entry:#?}",
                        key.operation_id.fmt_full()
                    );
                }
                Ok(())
            }
            "operation-fees" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: operation-fees <operation-id>"))?;
                let id = OperationId::from_str(id).context("invalid operation id")?;
                let fees = client.get_operation_fees(id).await?;
                println!("{fees:?}");
                Ok(())
            }
            "await-change" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: await-change <operation-id>"))?;
                let id = OperationId::from_str(id).context("invalid operation id")?;
                if await_change_outputs(&client, id).await? {
                    println!("ecash change outputs finalized");
                } else {
                    println!("timed out; withdrawal remains Submitted and will be retried");
                }
                Ok(())
            }
            "list-deposits" => {
                for row in collapse_deposit_rows(journal.deposits_all().await?) {
                    println!(
                        "{} {:?} {:?} {} funding_txid={:?} op={:?} net_credit={:?}",
                        row.id.0,
                        row.origin,
                        row.state,
                        row.amount,
                        row.funding_txid,
                        row.operation_id,
                        row.net_credit
                    );
                }
                Ok(())
            }
            "list-withdrawals" => {
                for row in journal.withdrawals_all().await? {
                    println!(
                        "{} {:?} {:?} {}",
                        row.id.0, row.origin, row.state, row.value
                    );
                }
                Ok(())
            }
            "withdraw" => {
                let address = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: withdraw <address> <sat>"))?;
                let amount = parse_sat(parts.next())?;
                command_withdraw(&client, &journal, federation_id, address, amount).await
            }
            "send" => {
                let address = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: send <address> <sat> <sat/vb>"))?;
                let amount = parse_sat(parts.next())?;
                let fee_rate_sat_vb: u64 = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: send <address> <sat> <sat/vb>"))?
                    .parse()
                    .context("invalid fee rate (sat/vB)")?;
                command_send_onchain(
                    &wallet,
                    &journal,
                    chain_network,
                    address,
                    amount,
                    fee_rate_sat_vb,
                )
                .await
            }
            "list-onchain" => command_list_onchain(&wallet, &journal).await,
            "ln-send" => {
                let invoice = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: ln-send <invoice> [gateway]"))?;
                let gateway = parts.next().map(lightning::parse_gateway).transpose()?;
                let (row, quote) =
                    lightning::prepare_send(&client, &journal, federation_id, invoice, gateway)
                        .await?;
                println!("ln send quote:");
                println!("  amount: {} msat", quote.amount_msat);
                println!("  gateway: {}", quote.gateway);
                println!("  gateway fee: {} msat", quote.gateway_fee_msat);
                println!("  federation fee: {} msat", quote.federation_fee_msat);
                println!("  total debit: {} msat", quote.total_debit_msat);
                println!("  row: {}", row.id.0);
                let row = lightning::execute_send(&client, &journal, &row.id).await?;
                print_lightning(&row);
                Ok(())
            }
            "ln-receive" => {
                let amount = parse_sat(parts.next())?;
                let expiry = parts
                    .next()
                    .map(|value| value.parse::<u32>().context("invalid expiry seconds"))
                    .transpose()?
                    .unwrap_or(3600);
                let gateway = parts.next().map(lightning::parse_gateway).transpose()?;
                let description = parts.collect::<Vec<_>>().join(" ");
                let (row, quote) = lightning::request_receive(
                    &client,
                    &journal,
                    federation_id,
                    fedimint_core::Amount::from_sats(amount.to_sat()),
                    expiry,
                    description,
                    gateway,
                )
                .await?;
                println!("ln receive quote:");
                println!("  amount: {} msat", quote.amount_msat);
                println!("  gateway: {}", quote.gateway);
                println!("  gateway fee: {} msat", quote.gateway_fee_msat);
                println!("  federation fee: {} msat", quote.federation_fee_msat);
                println!("  net: {} msat", quote.net_msat);
                println!("  expiry: {} s", quote.expiry_secs);
                println!("invoice: {}", row.invoice.as_deref().unwrap_or("<none>"));
                println!("row: {}", row.id.0);
                print_lightning(&row);
                Ok(())
            }
            "ln-status" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: ln-status <id>"))?;
                let id = LightningId(id.to_owned());
                let row = lightning::refresh_row(&client, &journal, &id).await?;
                print_lightning(&row);
                if let Some(invoice) = &row.invoice {
                    println!("  invoice: {invoice}");
                }
                Ok(())
            }
            "ln-list" => {
                for row in journal.lightning_all().await? {
                    let row = if row.state == berlin_wallet_core::journal::LightningState::Pending
                        && row.operation_id.is_some()
                    {
                        lightning::refresh_row(&client, &journal, &row.id)
                            .await
                            .unwrap_or(row)
                    } else {
                        row
                    };
                    print_lightning(&row);
                }
                Ok(())
            }
            "ln-gateways" => {
                for gateway in lightning::list_gateways(&client).await? {
                    println!("{gateway}");
                }
                Ok(())
            }
            "status" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: status <deposit-or-withdrawal-id>"))?;
                command_status(&journal, id).await
            }
            other => Err(anyhow!("unknown command: {other}")),
        };

        if let Err(err) = result {
            eprintln!("error: {err:#}");
        }
    }

    quiet_fedimint_task_logs();
    let _ = shutdown_tx.send(true);
    deposit_watchers.wait_all().await;
    // Drop the module handle before the client is unwrapped; it may hold a
    // clone of the client.
    drop(module);
    for mut task in tasks {
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
            // Wait for cancellation so the task releases its handles.
            let _ = task.await;
        }
    }
    match Arc::try_unwrap(client) {
        Ok(handle) => handle.shutdown().await,
        Err(_) => eprintln!(
            "warning: client handle is still referenced after task shutdown; \
             the database lock may remain held"
        ),
    }
    Ok(())
}

fn spawn_interrupt_listener(
    interrupt: watch::Sender<u64>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        break;
                    }
                    eprintln!();
                    eprintln!(
                        "interrupt: Ctrl+C cancels a running `watch`; type `quit` to exit the wallet"
                    );
                    interrupt.send_modify(|count| *count = count.wrapping_add(1));
                }
            }
        }
    })
}

#[cfg(unix)]
fn spawn_suspend_guard(mut shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let Ok(mut tstp) = signal(SignalKind::from_raw(libc::SIGTSTP)) else {
            return;
        };
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = tstp.recv() => {
                    eprintln!();
                    eprintln!(
                        "suspend is disabled: the wallet holds the database lock. \
                         Use `quit` to exit."
                    );
                }
            }
        }
    })
}

struct RestoreOptions<'a> {
    chain_source: Option<String>,
    expected_federation_id: Option<&'a str>,
    expected_identity: Option<&'a str>,
}

async fn restore(
    invite: &str,
    seed: &[u8; 64],
    root_secret: RootSecret,
    data_dir: &Path,
    recorded: Network,
    options: RestoreOptions<'_>,
) -> anyhow::Result<()> {
    let RestoreOptions {
        chain_source,
        expected_federation_id,
        expected_identity,
    } = options;
    let bdk_path = data_dir.join(BDK_DB_FILE);
    anyhow::ensure!(
        !bdk_path.exists(),
        "restore requires a fresh data dir (bdk.sqlite already exists)"
    );

    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let federation_id = invite.federation_id();
    let identity = seed::client_identity(&root_secret, federation_id)?;

    if let Some(expected) = expected_federation_id {
        anyhow::ensure!(
            federation_id.to_string() == expected,
            "restored federation id {federation_id} does not match expected {expected}"
        );
    }
    if let Some(expected) = expected_identity {
        anyhow::ensure!(
            identity == expected,
            "restored client identity {identity} does not match expected {expected}"
        );
    }

    println!("restore federation id: {federation_id}");
    println!("restore client identity: {identity}");

    let db = open_database(data_dir).await?;
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let client = recover_client(client_db(&db, 0), connectors, root_secret, invite).await?;
    anyhow::ensure!(
        client.federation_id() == federation_id,
        "recovered client federation id mismatch"
    );
    anyhow::ensure!(
        client.all_modules_usable(),
        "recovered client still has unavailable modules"
    );

    let chain_network = verified_client_network(&client, recorded).await?;
    init_network(data_dir, recorded)?;
    let chain = chain_config(data_dir, chain_source)?;
    let mut bdk = BdkWallet::open_or_create(&bdk_path, seed, chain_network, chain.clone())?;
    let last_active = bdk.full_scan().await?;
    let ecash = client.get_balance_for_unit(AmountUnit::BITCOIN).await?;
    let journal = Journal::new(db.clone(), vec![0x00]);
    let operation_log_empty = client
        .operation_log()
        .paginate_operations_rev(1, None)
        .await
        .is_empty();
    let deposit_count = journal.deposits_all().await?.len();
    let withdrawal_count = journal.withdrawals_all().await?.len();
    let summary = RestoreSummary::new(operation_log_empty, deposit_count, withdrawal_count);
    anyhow::ensure!(
        summary.is_balances_only(),
        "fresh restore is not balances-only; review the restore warning policy"
    );

    println!("restore modules usable: true");
    println!("restore recorded network: {recorded}");
    println!("restore chain-id network: {chain_network}");
    println!(
        "restore ecash balance: {} msat ({} sat)",
        ecash.msats,
        ecash.msats / 1000
    );
    println!("restore bdk full scan last active: {last_active:?}");
    println!("restore bdk balance: {}", bdk.balance());
    print_restore_summary(&summary);
    println!("restore assertions: ok");
    Ok(())
}

fn print_restore_summary(summary: &RestoreSummary) {
    println!(
        "restore operation log empty: {}",
        summary.operation_log_empty
    );
    println!(
        "restore history unavailable: {}",
        summary.history_unavailable
    );
    println!("restore journal deposits: {}", summary.deposit_count);
    println!("restore journal withdrawals: {}", summary.withdrawal_count);
    for warning in &summary.warnings {
        println!("restore warning: {warning}");
    }
}

async fn command_request_deposit(
    client: &Client,
    journal: &Journal,
    federation_id: fedimint_core::config::FederationId,
    amount: Amount,
) -> anyhow::Result<()> {
    let row = deposit::request_deposit(client, journal, federation_id, amount).await?;
    let module = client.get_first_module::<WalletClientModule>()?;
    let applied = deposit::run_address_task(journal, module.inner(), &row.id).await?;
    let row = journal
        .deposit(&row.id)
        .await?
        .ok_or_else(|| anyhow!("deposit row disappeared"))?;
    println!(
        "deposit {} state={:?} address={:?} (address write: {applied:?})",
        row.id.0, row.state, row.address
    );
    print_deposit_preview(&row);
    Ok(())
}

async fn command_fund_deposit(
    client: &ClientHandleArc,
    journal: &Journal,
    wallet: &Arc<Mutex<BdkWallet>>,
    federation_id: fedimint_core::config::FederationId,
    watchers: &sync::DepositWatchers,
    shutdown: watch::Receiver<bool>,
    id: &DepositId,
) -> anyhow::Result<()> {
    let module = client.get_first_module::<WalletClientModule>()?;
    let txid = {
        let mut wallet = wallet.lock().await;
        deposit::fund_deposit(journal, module.inner(), &mut wallet, id).await?
    };
    println!("funding txid: {txid}");
    if watchers.spawn(
        client.clone(),
        journal.clone(),
        federation_id,
        id.clone(),
        shutdown,
    ) {
        println!(
            "watching deposit {} in the background; `status {}` or `watch {}` to follow",
            id.0, id.0, id.0
        );
    }
    Ok(())
}

async fn command_request_withdraw(
    client: &Client,
    journal: &Journal,
    federation_id: fedimint_core::config::FederationId,
    address: &str,
    amount: Amount,
) -> anyhow::Result<()> {
    let min = min_withdrawal_sat(client).await?;
    anyhow::ensure!(
        amount.to_sat() >= min,
        "withdrawal amount {} sat is below the federation minimum {min} sat",
        amount.to_sat()
    );
    let row =
        withdrawal::request_withdrawal(client, journal, federation_id, address.to_owned(), amount)
            .await?;
    println!("withdrawal {} state={:?}", row.id.0, row.state);
    Ok(())
}

async fn command_send_withdraw(
    client: &Client,
    journal: &Journal,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    withdrawal::send_withdrawal(client, journal, id).await?;
    println!("{:#?}", journal.withdrawal(id).await?);
    Ok(())
}

async fn command_deposit(
    client: &ClientHandleArc,
    journal: &Journal,
    wallet: &Arc<Mutex<BdkWallet>>,
    federation_id: fedimint_core::config::FederationId,
    watchers: &sync::DepositWatchers,
    shutdown: watch::Receiver<bool>,
    amount: Amount,
) -> anyhow::Result<()> {
    let row = deposit::request_deposit(client, journal, federation_id, amount).await?;
    let module = client.get_first_module::<WalletClientModule>()?;
    let applied = deposit::run_address_task(journal, module.inner(), &row.id).await?;
    let row = journal
        .deposit(&row.id)
        .await?
        .ok_or_else(|| anyhow!("deposit row disappeared"))?;
    println!(
        "deposit {} state={:?} address={:?} (address write: {applied:?})",
        row.id.0, row.state, row.address
    );

    if row.address.is_some() {
        print_deposit_preview(&row);
        let txid = {
            let mut wallet = wallet.lock().await;
            deposit::fund_deposit(journal, module.inner(), &mut wallet, &row.id).await?
        };
        println!("funding txid: {txid}");
        if watchers.spawn(
            client.clone(),
            journal.clone(),
            federation_id,
            row.id.clone(),
            shutdown,
        ) {
            println!(
                "watching deposit {} in the background; `status {}` or `watch {}` to follow",
                row.id.0, row.id.0, row.id.0
            );
        }
    } else {
        println!("address not ready; rerun/retry later");
    }
    Ok(())
}

async fn command_watch(
    client: &Client,
    journal: &Journal,
    federation_id: fedimint_core::config::FederationId,
    id: &DepositId,
) -> anyhow::Result<()> {
    let row = journal
        .deposit(id)
        .await?
        .ok_or_else(|| anyhow!("deposit not found: {}", id.0))?;
    let position = row
        .position
        .ok_or_else(|| anyhow!("deposit has no event-log position"))?;
    let module = client.get_first_module::<WalletClientModule>()?;
    let started = Instant::now();

    // Progress belongs on stderr: stdout is the machine-readable final row.
    eprintln!("{}", format_watch_progress(&row, started.elapsed()));

    let watch =
        deposit::watch_deposit(client, journal, module.inner(), id, federation_id, position);
    tokio::pin!(watch);

    let mut heartbeat = tokio::time::interval(WATCH_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            result = &mut watch => {
                result?;
                break;
            }
            _ = heartbeat.tick() => {
                if let Some(row) = journal.deposit(id).await? {
                    eprintln!("{}", format_watch_progress(&row, started.elapsed()));
                }
            }
        }
    }

    if let Some(row) = journal.deposit(id).await? {
        eprintln!(
            "watch {} (deposit): finished elapsed={}s state={:?}",
            row.id.0,
            started.elapsed().as_secs(),
            row.state
        );
        print_deposit(&row);
    }
    Ok(())
}

fn format_watch_progress(row: &DepositRow, elapsed: Duration) -> String {
    let federation_position = row.next_position.or(row.position);
    let event_position = row
        .position
        .map(|position| format!("{position:?}"))
        .unwrap_or_else(|| "<none>".to_owned());
    let federation_position = federation_position
        .map(|position| format!("{position:?}"))
        .unwrap_or_else(|| "<none>".to_owned());
    format!(
        "watch {} (deposit): elapsed={}s state={:?} event_position={} \
         federation_event_position={} last_observed_position={} funding_txid={} operation_id={}",
        row.id.0,
        elapsed.as_secs(),
        row.state,
        event_position,
        federation_position,
        federation_position,
        row.funding_txid
            .map(|txid| txid.to_string())
            .unwrap_or_else(|| "<none>".to_owned()),
        row.operation_id
            .map(|operation_id| operation_id.fmt_full().to_string())
            .unwrap_or_else(|| "<none>".to_owned()),
    )
}

async fn command_withdraw(
    client: &Client,
    journal: &Journal,
    federation_id: fedimint_core::config::FederationId,
    address: &str,
    amount: Amount,
) -> anyhow::Result<()> {
    let min = min_withdrawal_sat(client).await?;
    anyhow::ensure!(
        amount.to_sat() >= min,
        "withdrawal amount {} sat is below the federation minimum {min} sat",
        amount.to_sat()
    );

    let row =
        withdrawal::request_withdrawal(client, journal, federation_id, address.to_owned(), amount)
            .await?;
    withdrawal::send_withdrawal(client, journal, &row.id).await?;
    println!("withdrawal {} -> {address}", row.id.0);
    println!("{:#?}", journal.withdrawal(&row.id).await?);
    Ok(())
}

async fn command_send_onchain(
    wallet: &Arc<Mutex<BdkWallet>>,
    journal: &Journal,
    network: bitcoin::Network,
    raw_address: &str,
    amount: Amount,
    fee_rate_sat_vb: u64,
) -> anyhow::Result<()> {
    let address = bitcoin::Address::from_str(raw_address).context("invalid Bitcoin address")?;
    let address = address
        .require_network(network)
        .context("address is for a different network")?;
    let fee_rate = bitcoin::FeeRate::from_sat_per_vb(fee_rate_sat_vb)
        .ok_or_else(|| anyhow!("fee rate is too high"))?;

    let mut wallet = wallet.lock().await;
    let plan = wallet.quote_send(&address, amount, fee_rate)?;
    let quote = &plan.quote;
    println!("send quote:");
    println!("  to: {}", quote.destination);
    println!("  amount: {} sat", quote.amount.to_sat());
    println!("  fee rate: {fee_rate_sat_vb} sat/vB");
    println!("  fee: {} sat", quote.fee.to_sat());
    println!("  total debit: {} sat", quote.total_debit.to_sat());
    println!("  change: {} sat", quote.change.to_sat());
    println!("  inputs: {}", quote.input_count);
    println!("  txid: {}", quote.txid);

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
    println!(
        "broadcast: {} ({} vB, fee {} sat)",
        signed.txid,
        signed.vsize,
        signed.quote.fee.to_sat()
    );
    Ok(())
}

async fn command_list_onchain(
    wallet: &Arc<Mutex<BdkWallet>>,
    journal: &Journal,
) -> anyhow::Result<()> {
    let rows = journal.onchain_sends_all().await?;
    if rows.is_empty() {
        println!("no journalled on-chain sends");
    } else {
        println!("journalled sends:");
        for row in rows {
            println!(
                "  {} {} {} fee={} sat vsize={} vB change={} sat state={:?}",
                row.id.0,
                row.txid,
                row.address,
                row.fee.to_sat(),
                row.vsize,
                row.change.to_sat(),
                row.state
            );
        }
    }

    println!("wallet transactions:");
    let wallet = wallet.lock().await;
    let entries = wallet.history();
    if entries.is_empty() {
        println!("  (none)");
    }
    for entry in entries {
        let position = match (entry.height, entry.confirmations) {
            (Some(height), Some(confirmations)) => {
                format!("height {height} ({confirmations} conf)")
            }
            _ => match entry.last_seen {
                Some(last_seen) => format!("unconfirmed (last seen {last_seen})"),
                None => "unconfirmed".to_owned(),
            },
        };
        let fee = entry
            .fee
            .map(|fee| format!("{} sat", fee.to_sat()))
            .unwrap_or_else(|| "unknown".to_owned());
        println!(
            "  {} {position} sent={} sat received={} sat fee={fee}",
            entry.txid,
            entry.sent.to_sat(),
            entry.received.to_sat()
        );
    }
    Ok(())
}

async fn command_status(journal: &Journal, id: &str) -> anyhow::Result<()> {
    if let Some(row) = journal.deposit(&DepositId(id.to_owned())).await? {
        print_deposit(&row);
        return Ok(());
    }
    if let Some(row) = journal.withdrawal(&WithdrawalId(id.to_owned())).await? {
        println!("{row:#?}");
        return Ok(());
    }
    anyhow::bail!("no deposit or withdrawal found with id {id}")
}

fn net_credit_display(net_credit: Option<fedimint_core::Amount>) -> String {
    match net_credit {
        Some(net_credit) => format!("{} msat", net_credit.msats),
        None => "pending (operation fee not available yet)".to_owned(),
    }
}

fn print_lightning(row: &LightningRow) {
    println!(
        "{} {:?} {:?} {} msat state={:?} op={:?} payment_hash={:?} preimage={:?}",
        row.id.0,
        row.kind,
        row.origin,
        row.amount_msat,
        row.state,
        row.operation_id,
        row.payment_hash,
        row.preimage
    );
    if let Some(gateway) = &row.gateway {
        println!("  gateway: {gateway}");
    }
    if let Some(fee) = row.gateway_fee_msat {
        println!("  gateway fee: {fee} msat");
    }
    if let Some(err) = &row.last_error {
        println!("  error: {err}");
    }
}

fn print_deposit_preview(row: &DepositRow) {
    println!("deposit preview:");
    println!("  amount: {} sat", row.amount.to_sat());
    match row.quoted_fee {
        Some(fee) => println!("  claim fee (on-chain): {} sat", fee.to_sat()),
        None => println!("  claim fee (on-chain): unavailable"),
    }
    match row.quoted_operation_fee {
        Some(fee) => println!(
            "  operation fee (mintv2 + walletv2 input): {} msat (estimate)",
            fee.msats
        ),
        None => println!("  operation fee: unavailable (reconciled after claim)"),
    }
    if let (Some(claim), Some(operation)) = (row.quoted_fee, row.quoted_operation_fee) {
        let gross_msat = row.amount.to_sat().saturating_mul(1_000);
        let net_msat = gross_msat
            .saturating_sub(claim.to_sat().saturating_mul(1_000))
            .saturating_sub(operation.msats);
        println!(
            "  estimated net: {net_msat} msat ({} sat)",
            net_msat / 1_000
        );
    }
}

fn print_deposit_actuals(row: &DepositRow) {
    if let Some(quote) = row.quoted_operation_fee {
        println!("operation fee (quoted estimate): {} msat", quote.msats);
    }
    if let Some(actual) = row.operation_fee {
        println!("operation fee (actual): {} msat", actual.msats);
    }
    if let (Some(quote), Some(actual)) = (row.quoted_operation_fee, row.operation_fee)
        && quote != actual
    {
        println!(
            "operation fee mismatch: quoted {} msat, actual {} msat",
            quote.msats, actual.msats
        );
    }
    if let (Some(quote), Some(actual)) = (row.quoted_fee, row.claim_fee)
        && quote != actual
    {
        println!(
            "claim fee mismatch: quoted {} sat, actual {} sat",
            quote.to_sat(),
            actual.to_sat()
        );
    }
}

fn print_deposit(row: &DepositRow) {
    println!("{row:#?}");
    println!("net_credit: {}", net_credit_display(row.net_credit));
    print_deposit_actuals(row);
    if row.funding_txid.is_some()
        && matches!(
            row.state,
            DepositState::AwaitingAddress | DepositState::AddressShown
        )
    {
        println!(
            "hint: funded but not claimed. `run` keeps a watcher alive for this row; \
             if it stays here, run `watch {}` and check that the federation sees the \
             funding transaction.",
            row.id.0
        );
    }
}

fn parse_sat(value: Option<&str>) -> anyhow::Result<Amount> {
    let value = value.ok_or_else(|| anyhow!("missing sat amount"))?;
    let sats: u64 = value.parse().context("invalid sat amount")?;
    Ok(Amount::from_sat(sats))
}

/// Remove `--network <value>` or `--network=<value>` from the argument vector
/// so the remaining positional indices stay stable.
fn take_network_flag(args: &mut Vec<String>) -> anyhow::Result<Option<String>> {
    if let Some(position) = args.iter().position(|arg| arg == "--network") {
        anyhow::ensure!(position + 1 < args.len(), "--network requires a value");
        let value = args.remove(position + 1);
        args.remove(position);
        return Ok(Some(value));
    }
    if let Some(position) = args.iter().position(|arg| arg.starts_with("--network=")) {
        let argument = args.remove(position);
        return Ok(Some(argument["--network=".len()..].to_owned()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use berlin_wallet_core::{
        HISTORY_UNAVAILABLE_WARNING, RECOVERY_BACKUP_WARNING, STRANDED_DEPOSIT_WARNING,
    };

    #[test]
    fn restore_warning_text_is_explicit() {
        let summary = RestoreSummary::new(true, 0, 0);
        assert!(summary.is_balances_only());
        assert_eq!(
            summary.warnings,
            vec![
                HISTORY_UNAVAILABLE_WARNING,
                STRANDED_DEPOSIT_WARNING,
                RECOVERY_BACKUP_WARNING,
            ]
        );
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.contains("A new deposit request does not recover it."))
        );
    }

    #[test]
    fn watch_progress_names_state_positions_and_funding() {
        let mut row = DepositRow::requested(
            DepositId("watch-progress".to_owned()),
            fedimint_core::config::FederationId::dummy(),
            fedimint_eventlog::EventLogId::LOG_START,
            bitcoin::Amount::from_sat(10_000),
        );
        row.state = DepositState::AddressShown;
        row.funding_txid = Some(
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .expect("txid"),
        );
        row.next_position = Some(fedimint_eventlog::EventLogId::LOG_START.next());

        let line = format_watch_progress(&row, Duration::from_secs(10));
        assert!(line.contains("watch watch-progress (deposit)"));
        assert!(line.contains("elapsed=10s"));
        assert!(line.contains("state=AddressShown"));
        assert!(line.contains("event_position=EventLogId(0)"));
        assert!(line.contains("federation_event_position=EventLogId(1)"));
        assert!(line.contains("last_observed_position=EventLogId(1)"));
        assert!(line.contains(
            "funding_txid=1111111111111111111111111111111111111111111111111111111111111111"
        ));
        assert!(line.contains("operation_id=<none>"));
    }

    #[test]
    fn network_flag_is_removed_before_positional_parsing() {
        let mut args = vec![
            "wallet".to_owned(),
            "init".to_owned(),
            "--network".to_owned(),
            "regtest".to_owned(),
            "esplora:http://127.0.0.1:3002".to_owned(),
        ];
        assert_eq!(
            take_network_flag(&mut args).expect("flag"),
            Some("regtest".to_owned())
        );
        assert_eq!(
            args,
            vec![
                "wallet".to_owned(),
                "init".to_owned(),
                "esplora:http://127.0.0.1:3002".to_owned()
            ]
        );

        let mut args = vec![
            "wallet".to_owned(),
            "run".to_owned(),
            "invite".to_owned(),
            "--network=signet".to_owned(),
        ];
        assert_eq!(
            take_network_flag(&mut args).expect("flag"),
            Some("signet".to_owned())
        );
        assert_eq!(
            args,
            vec!["wallet".to_owned(), "run".to_owned(), "invite".to_owned()]
        );

        let mut args = vec![
            "wallet".to_owned(),
            "init".to_owned(),
            "--network".to_owned(),
        ];
        assert!(take_network_flag(&mut args).is_err());
    }

    #[test]
    fn tracing_init_is_idempotent() {
        init_tracing();
        init_tracing();
    }

    #[test]
    fn data_dir_backup_round_trips_bdk_state_and_sidecars() {
        let base = std::env::temp_dir().join(format!(
            "berlin-wallet-backup-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = base.join("source");
        let backup = base.join("backup.age");
        let imported = base.join("imported");
        std::fs::create_dir_all(source.join("fedimint.rocksdb")).unwrap();
        berlin_wallet_core::seed::load_or_create_mnemonic(&source.join("mnemonic"), "wallet pass")
            .unwrap();
        std::fs::write(source.join("fedimint.rocksdb/CURRENT"), b"rocks").unwrap();

        let seed = [0x42u8; 64];
        let esplora = "http://127.0.0.1:1";
        let source_db = source.join(BDK_DB_FILE);
        let mut source_wallet = BdkWallet::open_or_create(
            &source_db,
            &seed,
            Network::Regtest,
            esplora.parse::<ChainBackendConfig>().unwrap(),
        )
        .unwrap();
        source_wallet.reveal_destination().unwrap();
        source_wallet.reveal_destination().unwrap();
        let source_balance = source_wallet.balance();
        drop(source_wallet);

        // Simulate SQLite WAL sidecars. A whole-directory backup must carry
        // them; a file-list copy would silently drop them.
        std::fs::write(source.join("bdk.sqlite-wal"), b"wal").unwrap();
        std::fs::write(source.join("bdk.sqlite-shm"), b"shm").unwrap();

        backup_data_dir(&source, &backup, "backup pass").expect("backup");
        import_data_dir(&imported, &backup, "backup pass").expect("import");

        assert!(imported.join("bdk.sqlite-wal").is_file());
        assert!(imported.join("bdk.sqlite-shm").is_file());
        // Remove synthetic sidecars before SQLite opens the imported DB.
        std::fs::remove_file(imported.join("bdk.sqlite-wal")).unwrap();
        std::fs::remove_file(imported.join("bdk.sqlite-shm")).unwrap();
        std::fs::remove_file(source.join("bdk.sqlite-wal")).unwrap();
        std::fs::remove_file(source.join("bdk.sqlite-shm")).unwrap();

        let mut imported_wallet = BdkWallet::open_or_create(
            &imported.join(BDK_DB_FILE),
            &seed,
            Network::Regtest,
            esplora.parse::<ChainBackendConfig>().unwrap(),
        )
        .unwrap();
        let imported_next = imported_wallet.reveal_destination().unwrap();
        let imported_balance = imported_wallet.balance();
        drop(imported_wallet);

        let mut source_wallet = BdkWallet::open_or_create(
            &source_db,
            &seed,
            Network::Regtest,
            esplora.parse::<ChainBackendConfig>().unwrap(),
        )
        .unwrap();
        let source_next = source_wallet.reveal_destination().unwrap();
        assert_eq!(imported_next, source_next, "address state must match");
        assert_eq!(
            format!("{:?}", imported_balance),
            format!("{:?}", source_balance),
            "BDK balance must match"
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn backup_refuses_a_locked_database() {
        let base = std::env::temp_dir().join(format!(
            "berlin-wallet-lock-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = base.join("source");
        let backup = base.join("backup.age");
        std::fs::create_dir_all(source.join("fedimint.rocksdb")).unwrap();
        berlin_wallet_core::seed::load_or_create_mnemonic(&source.join("mnemonic"), "wallet pass")
            .unwrap();
        std::fs::write(source.join(BDK_DB_FILE), b"bdk").unwrap();
        std::fs::write(source.join("fedimint.rocksdb/CURRENT"), b"rocks").unwrap();

        let lock_path = source.join("fedimint.rocksdb").with_extension("db.lock");
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)
            .unwrap();
        let _lock = fs_lock::FileLock::new_try_exclusive(lock_file).unwrap();

        let err =
            backup_data_dir(&source, &backup, "backup pass").expect_err("locked backup must fail");
        assert!(err.to_string().contains("database is locked"), "{err:#}");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn pending_net_credit_is_not_rendered_as_zero() {
        assert_eq!(
            net_credit_display(None),
            "pending (operation fee not available yet)"
        );
        assert_eq!(
            net_credit_display(Some(fedimint_core::Amount::from_msats(9_574_912))),
            "9574912 msat"
        );
    }
}
