use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet};
use berlin_wallet_core::client::{
    build_client, client_db, min_withdrawal_sat, open_rocksdb, recover_client,
};
use berlin_wallet_core::journal::{DepositRow, Journal};
use berlin_wallet_core::network::client_network;
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

const DEFAULT_NETWORK: Network = Network::Regtest;
const DEFAULT_ESPLORA_URL: &str = "http://127.0.0.1:3002";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
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
    let esplora_url = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| DEFAULT_ESPLORA_URL.to_owned());
    match command {
        None | Some("init") => {
            let mut bdk = BdkWallet::open_or_create(
                &data_dir.join(BDK_DB_FILE),
                &seed,
                DEFAULT_NETWORK,
                esplora_url,
            )?;
            println!("network: {DEFAULT_NETWORK}");
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
            join(invite, root_secret, &data_dir).await
        }
        Some("run") => {
            let invite = args
                .get(2)
                .ok_or_else(|| anyhow!("usage: berlin-wallet run <invite-code> [esplora-url]"))?;
            run(invite, &seed, root_secret, &data_dir, &esplora_url).await
        }
        Some("restore") => {
            let invite = args.get(2).ok_or_else(|| {
                anyhow!(
                    "usage: berlin-wallet restore <invite-code> [esplora-url] [expected-fed-id] [expected-client-identity]"
                )
            })?;
            let esplora_url = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| DEFAULT_ESPLORA_URL.to_owned());
            restore(
                invite,
                &seed,
                root_secret,
                &data_dir,
                &esplora_url,
                args.get(4).map(String::as_str),
                args.get(5).map(String::as_str),
            )
            .await
        }
        Some(other) => {
            print_help();
            anyhow::bail!("unknown command: {other}")
        }
    }
}

/// Copy the complete recovery artifact: mnemonic, fedimint RocksDB, and BDK SQLite.
///
/// Encryption is deliberately left to the caller (for example `age` or `gpg`);
/// this command only guarantees a consistent directory copy while the app is
/// stopped. The journal alone is not sufficient: the client DB contains the
/// walletv2 scanner/index state needed to reclaim deposits.
fn backup_data_dir(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.join("mnemonic").exists(),
        "backup source has no mnemonic: {}",
        source.display()
    );
    anyhow::ensure!(
        source.join("fedimint.rocksdb").is_dir(),
        "backup source has no fedimint.rocksdb: {}",
        source.display()
    );
    anyhow::ensure!(
        source.join(BDK_DB_FILE).is_file(),
        "backup source has no {}: {}",
        BDK_DB_FILE,
        source.display()
    );
    anyhow::ensure!(
        !destination.exists()
            || std::fs::read_dir(destination)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
        "backup destination must not exist or must be empty: {}",
        destination.display()
    );
    std::fs::create_dir_all(destination)?;

    copy_file(&source.join("mnemonic"), &destination.join("mnemonic"))?;
    copy_file(&source.join(BDK_DB_FILE), &destination.join(BDK_DB_FILE))?;
    copy_dir(
        &source.join("fedimint.rocksdb"),
        &destination.join("fedimint.rocksdb"),
    )?;

    println!("backup written: {}", destination.display());
    println!("contents: mnemonic, {BDK_DB_FILE}, fedimint.rocksdb");
    println!("stop the wallet before copying/archiving; encrypt the result with age/gpg.");
    println!(
        "restore by importing this directory with `restore-backup`, then run normally (never `restore`)."
    );
    Ok(())
}

/// Import a full directory backup into an empty data directory.
fn import_data_dir(destination: &Path, source: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.join("mnemonic").is_file(),
        "backup source has no mnemonic: {}",
        source.display()
    );
    anyhow::ensure!(
        source.join("fedimint.rocksdb").is_dir(),
        "backup source has no fedimint.rocksdb: {}",
        source.display()
    );
    anyhow::ensure!(
        source.join(BDK_DB_FILE).is_file(),
        "backup source has no {}: {}",
        BDK_DB_FILE,
        source.display()
    );
    anyhow::ensure!(
        !destination.exists()
            || std::fs::read_dir(destination)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
        "restore target must not exist or must be empty: {}",
        destination.display()
    );
    std::fs::create_dir_all(destination)?;

    copy_file(&source.join("mnemonic"), &destination.join("mnemonic"))?;
    copy_file(&source.join(BDK_DB_FILE), &destination.join(BDK_DB_FILE))?;
    copy_dir(
        &source.join("fedimint.rocksdb"),
        &destination.join("fedimint.rocksdb"),
    )?;

    println!("backup imported: {}", destination.display());
    println!(
        "run the wallet normally (not `restore`) so the retained client DB/scanner state is used."
    );
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn copy_dir(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

async fn open_database(data_dir: &Path) -> anyhow::Result<Database> {
    open_rocksdb(&data_dir.join("fedimint.rocksdb")).await
}

fn data_dir() -> PathBuf {
    std::env::var_os("BERLIN_WALLET_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".berlin-wallet"))
}

fn print_help() {
    println!();
    println!("commands:");
    println!("  init                                create/open the local wallet");
    println!("  join <invite>                       join a federation and print its config");
    println!(
        "  backup <dest-dir>                   copy mnemonic + client DB + BDK DB (app stopped)"
    );
    println!(
        "  restore-backup <src-dir>            import a full directory backup into an empty data dir"
    );
    println!("  restore <invite> [esplora] [fed] [identity]");
    println!("                                      recover on a fresh data dir + full BDK scan");
    println!("  run <invite> [esplora-url]          interactive manual-run session");
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

async fn join(invite: &str, root_secret: RootSecret, data_dir: &Path) -> anyhow::Result<()> {
    let db = open_database(data_dir).await?;
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let identity = seed::client_identity(&root_secret, invite.federation_id())?;
    let client = build_client(client_db(&db, 0), connectors, root_secret, invite).await?;

    let module = client.get_first_module::<WalletClientModule>()?;
    let chain_network = client_network(&client).await?;
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
    esplora_url: &str,
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
    let chain_network = client_network(&client).await?;
    let bdk = BdkWallet::open_or_create(
        &data_dir.join(BDK_DB_FILE),
        seed,
        chain_network,
        esplora_url,
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
    println!("chain-id network: {chain_network}");
    println!("config network: {}", module.get_network());
    println!("bdk address: {}", wallet.lock().await.reveal_destination()?);
    println!("min withdrawal: {} sat", min_withdrawal_sat(&client).await?);
    println!("type `help` for commands");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("berlin-wallet> ");
        io::stdout().flush()?;
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
            "sync" => {
                wallet.lock().await.sync().await?;
                println!("bdk synced");
                Ok(())
            }
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

async fn restore(
    invite: &str,
    seed: &[u8; 64],
    root_secret: RootSecret,
    data_dir: &Path,
    esplora_url: &str,
    expected_federation_id: Option<&str>,
    expected_identity: Option<&str>,
) -> anyhow::Result<()> {
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

    let chain_network = client_network(&client).await?;
    let mut bdk = BdkWallet::open_or_create(&bdk_path, seed, chain_network, esplora_url)?;
    let last_active = bdk.full_scan().await?;
    let ecash = client.get_balance_for_unit(AmountUnit::BITCOIN).await?;

    println!("restore modules usable: true");
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
        withdrawal::request_withdrawal(journal, federation_id, address.to_owned(), amount).await?;
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
        withdrawal::request_withdrawal(journal, federation_id, address.to_owned(), amount).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_backup_round_trips_the_full_artifact() {
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
        std::fs::write(source.join(BDK_DB_FILE), b"bdk").unwrap();
        std::fs::write(source.join("fedimint.rocksdb/CURRENT"), b"rocks").unwrap();

        backup_data_dir(&source, &backup).expect("backup");
        import_data_dir(&imported, &backup).expect("import");

        assert_eq!(
            std::fs::read(imported.join("mnemonic")).unwrap(),
            b"test mnemonic"
        );
        assert_eq!(std::fs::read(imported.join(BDK_DB_FILE)).unwrap(), b"bdk");
        assert_eq!(
            std::fs::read(imported.join("fedimint.rocksdb/CURRENT")).unwrap(),
            b"rocks"
        );
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
