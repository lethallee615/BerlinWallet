use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use berlin_wallet_core::backup::{backup_data_dir, import_data_dir};
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet};
use berlin_wallet_core::chain::ChainBackendConfig;
use berlin_wallet_core::client::{
    build_client, client_db, min_withdrawal_sat, open_rocksdb, recover_client,
};
use berlin_wallet_core::journal::{DepositRow, Journal};
use berlin_wallet_core::network::{
    NETWORK_FILE, init_network, network_from_chain_id, network_from_recorded, parse_network,
    require_recorded_network, verified_client_network,
};
use berlin_wallet_core::ops::{DepositId, WithdrawalId};
use berlin_wallet_core::seed;
use berlin_wallet_core::sync;
use berlin_wallet_core::withdrawal::await_change_outputs;
use berlin_wallet_core::{deposit, withdrawal};
use bitcoin::{Amount, Network};
use fedimint_client::Client;
use fedimint_client::RootSecret;
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::core::OperationId;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::AmountUnit;
use fedimint_walletv2_client::WalletClientModule;
use tokio::sync::{Mutex, watch};

const DEFAULT_ESPLORA_URL: &str = "http://127.0.0.1:3002";

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
            .ok_or_else(|| anyhow!("usage: berlin-wallet backup <destination-dir>"))?;
        return backup_data_dir(&data_dir, Path::new(destination));
    }
    if command == Some("restore-backup") {
        let source = args
            .get(2)
            .ok_or_else(|| anyhow!("usage: berlin-wallet restore-backup <source-dir>"))?;
        return import_data_dir(&data_dir, Path::new(source));
    }

    std::fs::create_dir_all(&data_dir)?;
    let mnemonic_path = data_dir.join("mnemonic");
    if command == Some("restore") && !mnemonic_path.exists() {
        anyhow::bail!(
            "restore requires an existing mnemonic at {}",
            mnemonic_path.display()
        );
    }
    let mnemonic = seed::load_or_create_mnemonic(&mnemonic_path)?;
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
            println!("mnemonic: {}", mnemonic_path.display());
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

/// Install a stderr tracing subscriber. `RUST_LOG` controls filtering and
/// defaults to `info`; `try_init` makes repeated calls harmless.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
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
        "  backup <dest-dir>                   copy mnemonic + client DB + BDK DB (app stopped)"
    );
    println!(
        "  restore-backup <src-dir>            import a full directory backup into an empty data dir"
    );
    println!("  restore --network <net> <invite> [chain-source] [fed] [identity]");
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
    println!("  watch <deposit-id>                  await the deposit's claim");
    println!("  request-withdraw <address> <sat>    create a withdrawal row");
    println!("  send-withdraw <withdrawal-id>       quote + send an existing row");
    println!("  cancel-deposit <deposit-id>         cancel an open deposit row");
    println!("  cancel-withdraw <withdrawal-id>     cancel a Quoting row");
    println!("  list-withdrawals                    list withdrawal rows");
    println!("  operations                          print recent operation-log entries");
    println!("  operation-fees <operation-id>       show operation fees");
    println!("  await-change <operation-id>         await primary change outputs");
    println!("  withdraw <address> <sat>            request + send in one step");
    println!("  status <id>                         print a deposit or withdrawal row");
    println!("  sync                                run one BDK Esplora sync");
    println!("  help");
    println!("  quit");
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
    tokio::spawn(sync::bdk_chain_task(wallet.clone(), shutdown_rx.clone()));
    tokio::spawn(sync::fedimint_sync_task(
        client.clone(),
        journal.clone(),
        shutdown_rx.clone(),
    ));
    tokio::spawn(sync::adoption_task(
        client.clone(),
        journal.clone(),
        federation_id,
        shutdown_rx,
    ));

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
                command_fund_deposit(&client, &journal, &wallet, &DepositId(id.to_owned())).await
            }
            "deposit" => {
                let amount = parse_sat(parts.next())?;
                command_deposit(&client, &journal, &wallet, federation_id, amount).await
            }
            "watch" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("usage: watch <deposit-id>"))?;
                command_watch(&client, &journal, federation_id, &DepositId(id.to_owned())).await
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
                    println!("change outputs finalized");
                } else {
                    println!("timed out; withdrawal remains Submitted and will be retried");
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

    let _ = shutdown_tx.send(true);
    Ok(())
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
    println!("restore assertions: ok");
    Ok(())
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
    Ok(())
}

async fn command_fund_deposit(
    client: &Client,
    journal: &Journal,
    wallet: &Arc<Mutex<BdkWallet>>,
    id: &DepositId,
) -> anyhow::Result<()> {
    let module = client.get_first_module::<WalletClientModule>()?;
    let mut wallet = wallet.lock().await;
    let txid = deposit::fund_deposit(journal, module.inner(), &mut wallet, id).await?;
    println!("funding txid: {txid}");
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
    client: &Client,
    journal: &Journal,
    wallet: &Arc<Mutex<BdkWallet>>,
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

    if row.address.is_some() {
        let mut wallet = wallet.lock().await;
        let txid = deposit::fund_deposit(journal, module.inner(), &mut wallet, &row.id).await?;
        println!("funding txid: {txid}");
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
    deposit::watch_deposit(client, journal, module.inner(), id, federation_id, position).await?;
    if let Some(row) = journal.deposit(id).await? {
        print_deposit(&row);
    }
    Ok(())
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

fn print_deposit(row: &DepositRow) {
    println!("{row:#?}");
    println!("net_credit: {}", net_credit_display(row.net_credit));
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
        let backup = base.join("backup");
        let imported = base.join("imported");
        std::fs::create_dir_all(source.join("fedimint.rocksdb")).unwrap();
        std::fs::write(source.join("mnemonic"), b"test mnemonic").unwrap();
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

        backup_data_dir(&source, &backup).expect("backup");
        import_data_dir(&imported, &backup).expect("import");

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
        let backup = base.join("backup");
        std::fs::create_dir_all(source.join("fedimint.rocksdb")).unwrap();
        std::fs::write(source.join("mnemonic"), b"test mnemonic").unwrap();
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

        let err = backup_data_dir(&source, &backup).expect_err("locked backup must fail");
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
