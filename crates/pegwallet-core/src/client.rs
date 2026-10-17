use std::sync::Arc;

use fedimint_client::{Client, ClientHandleArc, RootSecret};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_mint_client::MintClientInit;
use fedimint_walletv2_client::WalletClientInit;

/// Build a client with the walletv2 module registered.
pub async fn build_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let mut builder = Client::builder().await?;
    builder.with_module(MintClientInit);
    builder.with_module(WalletClientInit::default());

    let preview = builder.preview(connectors.clone(), &invite).await?;
    // `ClientPreview::join` returns a `ClientHandle`, not a `Client`.
    let client = preview.join(db, root_secret).await?;
    Ok(Arc::new(client))
}

/// App journal partition.
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

