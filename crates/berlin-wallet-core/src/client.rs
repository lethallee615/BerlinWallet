use std::sync::Arc;

use fedimint_client::{Client, ClientBuilder, ClientHandleArc, RootSecret};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::registry::ModuleDecoderRegistry;
use fedimint_lnv2_client::LightningClientInit as Lnv2ClientInit;
use fedimint_mintv2_client::MintClientInit as Mintv2ClientInit;
use fedimint_mintv2_common::KIND as MINT_V2_KIND;
use fedimint_mintv2_common::config::MintClientConfig;
use fedimint_walletv2_client::WalletClientInit;
use fedimint_walletv2_common::KIND as WALLET_V2_KIND;
use fedimint_walletv2_common::config::WalletClientConfig;

/// Build a client builder with the v2 module family registered.
async fn configured_builder() -> anyhow::Result<ClientBuilder> {
    let mut builder = Client::builder().await?;
    // The federation family is v2: mintv2 is the primary BTC module. The v1
    // mint client is intentionally not registered (it is not in the v2
    // federation config).
    builder.with_module(Mintv2ClientInit);
    // `WalletClientInit` is a unit struct (`#[derive(Default)]`). The real
    // `WalletClientConfig` is per-federation module config delivered through
    // `ClientModuleInitArgs::cfg()`, not a client-builder argument.
    builder.with_module(WalletClientInit);
    // lnv2 discovers the federation's vetted gateways at runtime; the default
    // init needs no gateway URL. Register only v2; the live federation has no
    // v1 `ln` module.
    builder.with_module(Lnv2ClientInit::default());
    Ok(builder)
}

/// Build a client with the walletv2 module registered.
pub async fn build_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let builder = configured_builder().await?;

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
    // `ClientBuilder::join`/`open` already call `start_executor` internally
    // (`fedimint-client/src/client/builder.rs:545,583`); calling it again panics
    // with "start_executor was called previously".
    Ok(Arc::new(client))
}

/// Open an existing client database without an invite code.
///
/// Used by mobile after `init()` on an already-initialized data directory.
/// `ClientBuilder::open` verifies the stored root-secret hash; the federation
/// id comes from the persisted `ClientConfig`.
pub async fn open_existing_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
) -> anyhow::Result<ClientHandleArc> {
    let config = Client::get_config_from_db(&db)
        .await
        .ok_or_else(|| anyhow::anyhow!("client database not initialized; use join()"))?;
    let federation_id = config.calculate_federation_id();
    let client = configured_builder()
        .await?
        .open(connectors, db, root_secret)
        .await?;
    anyhow::ensure!(
        client.federation_id() == federation_id,
        "opened client federation id mismatch"
    );
    Ok(Arc::new(client))
}

/// Restore a client from a mnemonic-derived root secret on a fresh database.
///
/// `ClientPreview::recover` runs module recovery but holds recovering modules
/// out of the registry. `Client::wait_for_all_recoveries` waits for progress;
/// `ClientHandle::shutdown` then stops the executor/task group, and
/// `ClientBuilder::open` reopens the same DB so all modules are usable.
///
/// The DB must be fresh (no persisted `ClientConfig`); otherwise this is an
/// ordinary boot and `build_client` should be used.
pub async fn recover_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let federation_id = invite.federation_id();
    anyhow::ensure!(
        Client::get_config_from_db(&db).await.is_none(),
        "restore requires a fresh client database (existing config found)"
    );

    let preview = configured_builder()
        .await?
        .preview(connectors.clone(), &invite)
        .await?;
    anyhow::ensure!(
        preview.config().calculate_federation_id() == federation_id,
        "invite federation id does not match preview config"
    );

    let handle = preview
        .recover(db.clone(), root_secret.clone(), None)
        .await?;
    anyhow::ensure!(
        handle.federation_id() == federation_id,
        "recovered client federation id mismatch"
    );

    handle.wait_for_all_recoveries().await?;
    let all_modules_usable_while_recovering = handle.all_modules_usable();

    // Modules held back during recovery become usable only after reopening.
    // Explicit shutdown stops the executor and joins the client task group
    // (bounded at 30 s) before the same DB is reopened.
    handle.shutdown().await;

    let reopened = configured_builder()
        .await?
        .open(connectors, db, root_secret)
        .await?;
    anyhow::ensure!(
        reopened.federation_id() == federation_id,
        "reopened client federation id mismatch"
    );
    anyhow::ensure!(
        reopened.all_modules_usable(),
        "recovery finished, but some modules are still unavailable after reopen"
    );
    // The mintv2 recovery reconstructs ecash by adding an output state machine
    // that issues the recovered notes. `wait_for_all_recoveries` only waits for
    // module recovery, not for that issuance; without this wait a caller that
    // reads the balance immediately after reopen sees 0 while the notes are
    // still pending. Bound the wait so an unreachable federation cannot hang a
    // restore forever.
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        reopened.wait_for_all_active_state_machines(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for recovered ecash notes to settle"))??;
    tracing::debug!(
        %federation_id,
        all_modules_usable_while_recovering,
        "client recovery completed and reopened"
    );
    Ok(Arc::new(reopened))
}

/// Read the per-federation walletv2 client config from the joined client.
///
/// `WalletClientInit` is a unit struct, so it can be registered directly; the
/// `WalletClientConfig` travels inside the federation's `ClientConfig` and is
/// exposed by `Client::config()`.
pub async fn wallet_client_config(client: &Client) -> anyhow::Result<WalletClientConfig> {
    let config = client.config().await;
    let (_, wallet_config) =
        config.get_first_module_by_kind::<WalletClientConfig>(WALLET_V2_KIND)?;
    Ok(wallet_config.clone())
}

/// The mintv2 client config, including its fee consensus.
pub async fn mint_client_config(client: &Client) -> anyhow::Result<MintClientConfig> {
    let config = client.config().await;
    let (_, mint_config) = config.get_first_module_by_kind::<MintClientConfig>(MINT_V2_KIND)?;
    Ok(mint_config.clone())
}

/// Best available federation operation-fee preview for a deposit of `amount`.
///
/// The claim transaction has a walletv2 input and a mintv2 output, so the
/// estimate sums both fee consensuses. It is still labelled an estimate: the
/// exact fee is read from `Client::get_operation_fees` after the claim and may
/// differ by rounding or dust.
pub async fn deposit_operation_fee_estimate(
    client: &Client,
    amount: bitcoin::Amount,
) -> Option<fedimint_core::Amount> {
    let wallet = wallet_client_config(client).await.ok()?;
    let mint = mint_client_config(client).await.ok()?;
    let gross = fedimint_core::Amount::from_sats(amount.to_sat());
    Some(fedimint_core::Amount::from_msats(
        wallet
            .fee_consensus
            .fee(gross)
            .msats
            .saturating_add(mint.fee_consensus.fee(gross).msats),
    ))
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
    // This is the `db_no_decoders` input to ClientBuilder::join/open. The
    // builder installs module decoders with `Database::with_decoders` after it
    // has decoded the federation config (`client/builder.rs:628-630`).
    Ok(Database::new(raw, ModuleDecoderRegistry::default()))
}

/// Open the hand-rolled SQLite backend (kept for mobile/embedded hosts).
pub fn open_sqlite(path: &std::path::Path) -> anyhow::Result<Database> {
    let raw = crate::sqlite::SqliteDb::open(path)?;
    // Same `db_no_decoders` contract as `open_rocksdb`.
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
    use crate::journal::{DepositRow, Journal, LightningRow, OnchainSendRow};
    use crate::ops::{DepositId, LightningId, OnchainSendId};

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rocksdb_lightning_row_survives_reopen() {
        let path =
            std::env::temp_dir().join(format!("berlin-wallet-rocksdb-{}", uuid::Uuid::now_v7()));
        let id = LightningId::new();

        {
            let db = open_rocksdb(&path).await.expect("open rocksdb");
            let journal = Journal::new(db, vec![0x00]);
            let row = LightningRow::requested(
                id.clone(),
                FederationId::dummy(),
                crate::journal::LightningKind::Send,
                100_000,
            );
            journal
                .insert_lightning(&row)
                .await
                .expect("insert lightning row");
        }

        let db = open_rocksdb(&path).await.expect("reopen rocksdb");
        let journal = Journal::new(db, vec![0x00]);
        let rows = journal.lightning_all().await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].state, crate::journal::LightningState::Pending);

        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rocksdb_onchain_send_survives_reopen() {
        let path =
            std::env::temp_dir().join(format!("berlin-wallet-rocksdb-{}", uuid::Uuid::now_v7()));
        let id = OnchainSendId::new();
        let txid: bitcoin::Txid =
            "0000000000000000000000000000000000000000000000000000000000000066"
                .parse()
                .expect("valid txid");

        {
            let db = open_rocksdb(&path).await.expect("open rocksdb");
            let journal = Journal::new(db, vec![0x00]);
            let row = OnchainSendRow::broadcast(
                id.clone(),
                "bcrt1qdpejjzcmhslvkf0katf7wnjumg8qlj50rxarzj66tfl0vl8cgdtsyy7fjf".to_owned(),
                bitcoin::Amount::from_sat(10_000),
                2,
                bitcoin::Amount::from_sat(300),
                150,
                bitcoin::Amount::from_sat(39_700),
                txid,
            );
            journal
                .insert_onchain_send(&row)
                .await
                .expect("insert on-chain send");
        }

        let db = open_rocksdb(&path).await.expect("reopen rocksdb");
        let journal = Journal::new(db, vec![0x00]);
        let rows = journal.onchain_sends_all().await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].txid, txid);

        let _ = std::fs::remove_dir_all(path);
    }
}
