use std::io::{Read, Write};
use std::path::Path;

use bip39::{Language, Mnemonic};
use bitcoin::hashes::Hash as _;
use fedimint_client::RootSecret;
use fedimint_client::secret::RootSecretStrategy;
use fedimint_core::config::FederationId;
use fedimint_core::encoding::{Decodable, DecodeError, Encodable};
use fedimint_core::module::registry::ModuleRegistry;
use fedimint_derive_secret::{ChildId, DerivableSecret};
use rand::{CryptoRng, RngCore};

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

/// BIP39 seed (64 bytes, empty passphrase) used for the BDK BIP32/BIP84
/// hierarchy.
pub fn mnemonic_seed(mnemonic: &Mnemonic) -> [u8; 64] {
    mnemonic.to_seed("")
}

/// Canonical fedimint BIP39 root-secret strategy.
///
/// This mirrors `fedimint-bip39` at fedimint tag `v0.12.0` (the crate is not
/// published on crates.io for 0.12.0):
///
/// ```text
/// DerivableSecret::new_root(
///     mnemonic.to_seed_normalized("").as_ref(),
///     b"Fedimint Client Salt",
/// )
/// ```
///
/// `PlainRootSecretStrategy` performs the same inner operation on the already
/// extracted 64-byte seed; the test below asserts the two are identical.
#[derive(Debug)]
pub struct Bip39RootSecretStrategy<const WORD_COUNT: usize = 12>;

impl<const WORD_COUNT: usize> RootSecretStrategy for Bip39RootSecretStrategy<WORD_COUNT> {
    type Encoding = Mnemonic;

    fn to_root_secret(secret: &Self::Encoding) -> DerivableSecret {
        const FEDIMINT_CLIENT_NONCE: &[u8] = b"Fedimint Client Salt";
        const EMPTY_PASSPHRASE: &str = "";

        DerivableSecret::new_root(
            secret.to_seed_normalized(EMPTY_PASSPHRASE).as_ref(),
            FEDIMINT_CLIENT_NONCE,
        )
    }

    fn consensus_encode(secret: &Self::Encoding, writer: &mut impl Write) -> std::io::Result<()> {
        secret.to_entropy().consensus_encode(writer)
    }

    fn consensus_decode_partial(reader: &mut impl Read) -> Result<Self::Encoding, DecodeError> {
        let bytes = Vec::<u8>::consensus_decode_partial(reader, &ModuleRegistry::default())?;
        Mnemonic::from_entropy(&bytes).map_err(DecodeError::from_err)
    }

    fn random<R>(rng: &mut R) -> Self::Encoding
    where
        R: RngCore + CryptoRng,
    {
        Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT)
            .expect("Failed to generate mnemonic, bad word count")
    }
}

/// Canonical root secret from a BIP39 mnemonic.
pub fn root_secret_from_mnemonic(mnemonic: &Mnemonic) -> RootSecret {
    RootSecret::StandardDoubleDerive(Bip39RootSecretStrategy::<12>::to_root_secret(mnemonic))
}

/// Stable, non-secret client identity for one federation.
///
/// Mirrors `fedimint_client_module::secret::get_default_client_secret`:
/// `global.child(0).federation_key(fed).child(0).child(0)`, then hashes the
/// derived client secret. This is what must match after restore; it is safe to
/// print because only the SHA-256 digest leaves the process.
pub fn client_identity(
    root_secret: &RootSecret,
    federation_id: FederationId,
) -> anyhow::Result<String> {
    let RootSecret::StandardDoubleDerive(global_root_secret) = root_secret else {
        anyhow::bail!("client identity is only defined for StandardDoubleDerive root secrets");
    };
    let multi_federation = global_root_secret.child_key(ChildId(0));
    let federation_root = multi_federation.federation_key(&federation_id);
    let wallet_root = federation_root.child_key(ChildId(0));
    let client_root = wallet_root.child_key(ChildId(0));
    let secret_bytes = client_root.to_random_bytes::<32>();
    Ok(bitcoin::hashes::sha256::Hash::hash(&secret_bytes).to_string())
}

#[cfg(test)]
mod tests {
    use fedimint_client::secret::PlainRootSecretStrategy;

    use super::*;

    #[test]
    fn bip39_strategy_matches_plain_seed_strategy() {
        // Standard BIP39 test vector (12 words, empty passphrase).
        let mnemonic = Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .expect("valid mnemonic");

        let canonical = Bip39RootSecretStrategy::<12>::to_root_secret(&mnemonic);
        let plain = PlainRootSecretStrategy::to_root_secret(&mnemonic.to_seed(""));

        assert_eq!(
            canonical.to_random_bytes::<32>(),
            plain.to_random_bytes::<32>(),
            "canonical BIP39 and plain-seed strategies must derive the same root"
        );
    }

    #[test]
    fn client_identity_is_stable_for_the_same_seed() {
        let mnemonic = Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .expect("valid mnemonic");
        let root = root_secret_from_mnemonic(&mnemonic);
        let federation_id = FederationId::dummy();
        let first = client_identity(&root, federation_id).expect("identity");
        let second = client_identity(&root, federation_id).expect("identity");
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn bip39_matches_canonical_golden_vector() {
        // Standard BIP39 test vector, empty passphrase. This is the canonical
        // fedimint-bip39 v0.12.0 derivation:
        // DerivableSecret::new_root(to_seed_normalized(""),
        // b"Fedimint Client Salt"), then the raw-bytes child derivation.
        let mnemonic = Mnemonic::parse(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .expect("valid mnemonic");
        let root = Bip39RootSecretStrategy::<12>::to_root_secret(&mnemonic);
        assert_eq!(
            root.to_random_bytes::<32>(),
            [
                0x6c, 0x63, 0x84, 0x4c, 0xf0, 0x9e, 0xf2, 0x98, 0xe8, 0x4a, 0x57, 0x50, 0xc2, 0x7d,
                0x9d, 0xc0, 0x5d, 0xcf, 0x45, 0xc9, 0x9c, 0xb8, 0x2e, 0x0c, 0x27, 0xf9, 0x03, 0xaa,
                0xc3, 0xa5, 0x7d, 0xba,
            ],
            "canonical BIP39 root-secret bytes changed"
        );
    }
}
