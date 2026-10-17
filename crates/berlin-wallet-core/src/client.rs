use std::sync::Arc;

use fedimint_client::{Client, ClientHandleArc, RootSecret};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
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

    let preview = builder.preview(connectors.clone(), &invite).await?;
    // `ClientPreview::join` returns a `ClientHandle`, not a `Client`.
    let client = preview.join(db, root_secret).await?;
    Ok(Arc::new(client))
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

