use std::sync::Arc;

use fedimint_client::{Client, ClientHandleArc, RootSecret};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::registry::ModuleDecoderRegistry;
use fedimint_mint_client::MintClientInit;
use fedimint_walletv2_client::WalletClientInit;
use fedimint_walletv2_common::KIND as WALLET_V2_KIND;
use fedimint_walletv2_common::config::WalletClientConfig;

/// Build a client with the walletv2 module registered.
pub async fn build_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let mut builder = Client::builder().await?;
    builder.with_module(MintClientInit);
    // `WalletClientInit` is a unit struct (`#[derive(Default)]`). The real
    // `WalletClientConfig` is per-federation module config delivered through
    // `ClientModuleInitArgs::cfg()`, not a client-builder argument.
    builder.with_module(WalletClientInit::default());

    // A restarted process must open the existing client rather than trying to
    // join a fresh one. `ClientBuilder::open` also verifies the root-secret
    // hash against the value stored on the first join.
    let client = if let Some(existing) = Client::get_config_from_db(&db).await {
        let existing_federation = existing.calculate_federation_id();
        anyhow::ensure!(
            existing_federation == invite.federation_id(),
            "database contains federation {existing_federation}, but the invite is for {}",
            invite.federation_id()
        );
        builder.open(connectors, db, root_secret).await?
    } else {
        let preview = builder.preview(connectors, &invite).await?;
        // `ClientPreview::join` returns a `ClientHandle`, not a `Client`.
        preview.join(db, root_secret).await?
    };
    let client = Arc::new(client);
    client.start_executor();
    Ok(client)
}

/// Read the per-federation walletv2 client config from the joined client.
///
/// `WalletClientInit` is a unit struct, so `WalletClientInit::default()` is
/// correct; the `WalletClientConfig` travels inside the federation's
/// `ClientConfig` and is exposed by `Client::config()`.
pub async fn wallet_client_config(client: &Client) -> anyhow::Result<WalletClientConfig> {
    let config = client.config().await;
    let (_, wallet_config) =
        config.get_first_module_by_kind::<WalletClientConfig>(WALLET_V2_KIND)?;
    Ok(wallet_config.clone())
}

/// Minimum on-chain withdrawal value in sats, as enforced by the federation's
/// decoded `WalletClientConfig.dust_limit`. The UI should display this value and
/// reject or disable smaller withdrawals before calling `withdraw`.
pub async fn min_withdrawal_sat(client: &Client) -> anyhow::Result<u64> {
    Ok(wallet_client_config(client).await?.dust_limit.to_sat())
}

/// App journal partition.
/// Open the primary persistent RocksDB-backed database.
///
/// `fedimint-rocksdb` is not published at 0.12.0; the workspace patches every
/// fedimint crate to the `v0.12.0` Git tag so `fedimint-core` unifies with the
/// Git backend crates.
pub async fn open_rocksdb(path: &std::path::Path) -> anyhow::Result<Database> {
    let raw = fedimint_rocksdb::RocksDb::build(path).open().await?;
    Ok(Database::new(raw, ModuleDecoderRegistry::default()))
}

/// Open the hand-rolled SQLite backend (kept for mobile/embedded hosts).
pub fn open_sqlite(path: &std::path::Path) -> anyhow::Result<Database> {
    let raw = crate::sqlite::SqliteDb::open(path)?;
    Ok(Database::new(raw, ModuleDecoderRegistry::default()))
}

pub fn journal_db(db: &Database) -> Database {
    db.with_prefix(vec![0x00])
}

/// Fedimint client `i` partition: `[0x01] ++ u32_le(db_prefix)`.
pub fn client_db(db: &Database, db_prefix: u32) -> Database {
    let mut prefix = vec![0x01];
    prefix.extend_from_slice(&db_prefix.to_le_bytes());
    db.with_prefix(prefix)
}

/// `Client::start_executor` takes `self: &Arc<Self>`; `ClientHandle` exposes
/// its own `start_executor(&self)` which reaches the inner `Arc<Client>`.
/// (`ClientHandle::inner_arc` is `#[cfg(feature = "uniffi")]` only.)
pub fn start_executor(handle: &ClientHandleArc) {
    handle.start_executor();
}

#[cfg(test)]
mod tests {
    use fedimint_core::config::FederationId;
    use fedimint_eventlog::EventLogId;

    use super::*;
    use crate::journal::{DepositRow, Journal};
    use crate::ops::DepositId;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rocksdb_journal_rows_survive_reopen() {
        let path =
            std::env::temp_dir().join(format!("berlin-wallet-rocksdb-{}", uuid::Uuid::now_v7()));
        let federation_id = FederationId::dummy();
        let id = DepositId::new();
        let amount = bitcoin::Amount::from_sat(75_000);

        {
            let db = open_rocksdb(&path).await.expect("open rocksdb");
            let journal = Journal::new(db, vec![0x00]);
            let row =
                DepositRow::requested(id.clone(), federation_id, EventLogId::LOG_START, amount);
            journal.insert_deposit(&row).await.expect("insert");
        }

        let db = open_rocksdb(&path).await.expect("reopen rocksdb");
        let journal = Journal::new(db, vec![0x00]);
        let row = journal
            .deposit(&id)
            .await
            .expect("read")
            .expect("row must survive reopen");
        assert_eq!(row.id, id);
        assert_eq!(row.amount, amount);
        assert_eq!(row.federation_id, federation_id);

        let _ = std::fs::remove_dir_all(path);
    }
}
