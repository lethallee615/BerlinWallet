use std::path::Path;

use bip39::Mnemonic;
use fedimint_client::RootSecret;
use fedimint_client_module::secret::{PlainRootSecretStrategy, RootSecretStrategy};

/// Load the wallet mnemonic from `path`, creating and persisting a fresh
/// 12-word BIP39 mnemonic if the file does not exist.
pub fn load_or_create_mnemonic(path: &Path) -> anyhow::Result<Mnemonic> {
    if path.exists() {
        let phrase = std::fs::read_to_string(path)?;
        Mnemonic::parse(phrase.trim())
            .map_err(|err| anyhow::anyhow!("invalid mnemonic in {}: {err}", path.display()))
    } else {
        let mnemonic = Mnemonic::generate(12)
            .map_err(|err| anyhow::anyhow!("could not generate mnemonic: {err}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, mnemonic.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(mnemonic)
    }
}

/// BIP39 seed (64 bytes, empty passphrase).
pub fn mnemonic_seed(mnemonic: &Mnemonic) -> [u8; 64] {
    mnemonic.to_seed("")
}

/// Fedimint root secret.
///
/// `Bip39RootSecretStrategy` is not published for fedimint 0.12.0. The BIP39
/// seed is fed to the real `PlainRootSecretStrategy`, then wrapped in the
/// standard double-derive `RootSecret`.
pub fn root_secret_from_seed(seed: &[u8; 64]) -> RootSecret {
    RootSecret::StandardDoubleDerive(PlainRootSecretStrategy::to_root_secret(seed))
}

pub fn root_secret_from_mnemonic(mnemonic: &Mnemonic) -> RootSecret {
    root_secret_from_seed(&mnemonic_seed(mnemonic))
}
