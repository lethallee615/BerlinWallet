use bdk_wallet::signer::SignersContainer;
use bdk_wallet::{KeychainKind, SignOptions, Wallet};

/// Peek/reveal the next BDK external address for a withdrawal.
pub fn reveal_destination(wallet: &mut Wallet) -> bitcoin::Address {
    wallet.reveal_next_address(KeychainKind::External).address
}

/// Build, sign and finalize a BDK transaction paying `destination`.
///
/// The deposit address returned by `WalletClientModule::receive()` is a
/// `bitcoin::Address`; `TxBuilder::add_recipient` takes a `ScriptBuf`, so the
/// sketch's "pay `addr`" becomes `addr.script_pubkey()`.
pub fn build_and_finalize(
    wallet: &mut Wallet,
    destination: &bitcoin::Address,
    amount: bitcoin::Amount,
    signers: &[&SignersContainer],
) -> anyhow::Result<bitcoin::Transaction> {
    let mut builder = wallet.build_tx();
    builder.add_recipient(destination.script_pubkey(), amount);
    let mut psbt = builder.finish()?;

    let _ = wallet.sign_with_signers(&mut psbt, signers, SignOptions::default())?;
    let _ = wallet.finalize_psbt(&mut psbt, SignOptions::default())?;

    Ok(psbt.extract_tx()?)
}
