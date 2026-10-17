use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use berlin_wallet_core::bdk::{BDK_DB_FILE, BdkWallet};
use berlin_wallet_core::client::{build_client, client_db, min_withdrawal_sat};
use berlin_wallet_core::journal::Journal;
use berlin_wallet_core::ops::{DepositId, WithdrawalId};
use berlin_wallet_core::seed;
use berlin_wallet_core::sync;
use berlin_wallet_core::{deposit, withdrawal};
use bitcoin::{Amount, Network};
use fedimint_client::Client;
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;
use fedimint_core::db::mem_impl::MemDatabase;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::registry::ModuleDecoderRegistry;
use fedimint_walletv2_client::WalletClientModule;
use tokio::sync::{Mutex, watch};

const NETWORK: Network = Network::Regtest;
const DEFAULT_ESPLORA_URL: &str = "http://127.0.0.1:3002";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let mnemonic_path = data_dir.join("mnemonic");
    let mnemonic = seed::load_or_create_mnemonic(&mnemonic_path)?;
    let seed = seed::mnemonic_seed(&mnemonic);
    let esplora_url = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| DEFAULT_ESPLORA_URL.to_owned());
    let bdk = BdkWallet::open_or_create(&data_dir.join(BDK_DB_FILE), &seed, NETWORK, esplora_url)?;

    match args.get(1).map(String::as_str) {
        None | Some("init") => {
            let mut bdk = bdk;
            println!("network: {NETWORK}");
            println!("data dir: {}", data_dir.display());
            println!("mnemonic: {}", mnemonic_path.display());
            println!("bdk db: {}", data_dir.join(BDK_DB_FILE).display());
            println!("bdk address: {}", bdk.reveal_destination()?);
            println!("bdk balance: {}", bdk.balance());
            print_help();
            Ok(())
        }
        Some("join") => {
            let invite = args
                .get(2)
                .ok_or_else(|| anyhow!("usage: berlin-wallet join <invite-code>"))?;
            join(invite, &seed).await
        }
        Some("run") => {
            let invite = args
                .get(2)
                .ok_or_else(|| anyhow!("usage: berlin-wallet run <invite-code> [esplora-url]"))?;
            run(invite, bdk, &seed).await
        }
        Some(other) => {
            print_help();
            anyhow::bail!("unknown command: {other}")
        }
    }
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
    println!("  run <invite> [esplora-url]          interactive manual-run session");
    println!();
    println!("interactive commands:");
    println!("  balance                             BDK balance");
    println!("  address                             reveal the next BDK address");
    println!("  min                                 federation minimum withdrawal (sat)");
    println!("  request-deposit <sat>               request a deposit address");
    println!("  fund-deposit <deposit-id>           fund an address-ready deposit");
    println!("  deposit <sat>                       request + fund in one step");
    println!("  watch <deposit-id>                  await the deposit's claim");
    println!("  request-withdraw <address> <sat>    create a withdrawal row");
    println!("  send-withdraw <withdrawal-id>       quote + send an existing row");
    println!("  withdraw <address> <sat>            request + send in one step");
    println!("  status <id>                         print a deposit or withdrawal row");
    println!("  sync                                run one BDK Esplora sync");
    println!("  help");
    println!("  quit");
}

async fn join(invite: &str, seed: &[u8; 64]) -> anyhow::Result<()> {
    let db = Database::new(MemDatabase::new(), ModuleDecoderRegistry::default());
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let client = build_client(
        client_db(&db, 0),
        connectors,
        seed::root_secret_from_seed(seed),
        invite,
    )
    .await?;

    let module = client.get_first_module::<WalletClientModule>()?;
    println!("federation id: {}", client.federation_id());
    println!("network: {}", module.get_network());
    println!("min withdrawal: {} sat", min_withdrawal_sat(&client).await?);
    Ok(())
}

async fn run(invite: &str, bdk: BdkWallet, seed: &[u8; 64]) -> anyhow::Result<()> {
    let db = Database::new(MemDatabase::new(), ModuleDecoderRegistry::default());
    let journal = Journal::new(db.clone(), vec![0x00]);
    let connectors = ConnectorRegistry::build_from_client_defaults()
        .bind()
        .await?;
    let invite = InviteCode::from_str(invite).context("invalid invite code")?;
    let client = build_client(
        client_db(&db, 0),
        connectors,
        seed::root_secret_from_seed(seed),
        invite,
    )
    .await?;
    let federation_id = client.federation_id();
    let wallet = Arc::new(Mutex::new(bdk));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(sync::bdk_chain_task(wallet.clone(), shutdown_rx.clone()));
    tokio::spawn(sync::fedimint_sync_task(
        client.clone(),
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
    println!("network: {}", module.get_network());
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
    println!("{:#?}", journal.deposit(id).await?);
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
        println!("{row:#?}");
        return Ok(());
    }
    if let Some(row) = journal.withdrawal(&WithdrawalId(id.to_owned())).await? {
        println!("{row:#?}");
        return Ok(());
    }
    anyhow::bail!("no deposit or withdrawal found with id {id}")
}

fn parse_sat(value: Option<&str>) -> anyhow::Result<Amount> {
    let value = value.ok_or_else(|| anyhow!("missing sat amount"))?;
    let sats: u64 = value.parse().context("invalid sat amount")?;
    Ok(Amount::from_sat(sats))
}
