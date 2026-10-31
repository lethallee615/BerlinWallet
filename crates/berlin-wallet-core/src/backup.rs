//! Full data-directory backup/import helpers.
//!
//! The artifact is the entire directory: encrypted mnemonic, `chain.json` (the
//! selected chain source, when the data directory has recorded one), BDK SQLite
//! (including `-wal`/`-shm`), and the fedimint RocksDB directory. `chain.json`
//! is not a precondition — it is copied only because the archive takes every
//! entry — but dropping it would silently reset the chain backend on restore.
//! A hand-picked file list can lose it, WAL commits, or RocksDB sidecars.
//!
//! Backups are a single age passphrase-encrypted tar file. age provides an
//! authenticated, versioned container; a manifest inside the tar carries the
//! Berlin Wallet backup format version. The live `fedimint.rocksdb` and
//! `bdk.sqlite` files are not application-encrypted; full-disk encryption is
//! the supported at-rest boundary for those (see `ARCHITECTURE.md`).

use std::io::{BufReader, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::bdk::BDK_DB_FILE;

/// Manifest file written at the archive root.
pub const BACKUP_MANIFEST_FILE: &str = ".berlin-wallet-backup.json";
/// Manifest format identifier.
pub const BACKUP_FORMAT: &str = "berlin-wallet-backup";
/// Current backup format version.
pub const BACKUP_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    format: String,
    version: u32,
    created_at: u64,
}

/// Copy the complete data directory into a passphrase-encrypted age archive.
///
/// The destination must be a new file path (not a directory). The operation
/// holds the fedimint DB lock, writes a sibling temporary tar and age file, and
/// renames the completed archive into place. A crash before the rename leaves
/// the destination absent and a uniquely named temporary file that a later
/// backup ignores.
pub fn backup_data_dir(source: &Path, destination: &Path, passphrase: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !passphrase.is_empty(),
        "backup passphrase must not be empty"
    );
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
        crate::seed::is_encrypted_mnemonic(&std::fs::read(source.join("mnemonic"))?),
        "backup source mnemonic is legacy plaintext; run the wallet or init with a \
         passphrase once to migrate it before backup"
    );
    anyhow::ensure!(
        !destination.exists(),
        "backup destination already exists: {}",
        destination.display()
    );
    anyhow::ensure!(
        !destination.starts_with(source),
        "backup destination must not be inside the data directory"
    );
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    // Hold the same advisory lock fedimint-rocksdb uses. LockedBuilder blocks
    // on contention; we want a loud failure instead of copying a live DB.
    let db_path = source.join("fedimint.rocksdb");
    let lock_path = db_path.with_extension("db.lock");
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    let _lock = match fs_lock::FileLock::new_try_exclusive(lock_file) {
        Ok(lock) => lock,
        Err((_file, None)) => anyhow::bail!(
            "database is locked ({}); stop the wallet before running backup",
            lock_path.display()
        ),
        Err((_file, Some(err))) => {
            return Err(err).context("could not acquire the database lock");
        }
    };

    let temp_tar = temp_sibling(destination, "tar");
    let temp_archive = temp_sibling(destination, "age");
    let result = (|| -> anyhow::Result<()> {
        let tar_file = create_private_file(&temp_tar)?;
        let mut builder = tar::Builder::new(tar_file);
        let manifest = BackupManifest {
            format: BACKUP_FORMAT.to_owned(),
            version: BACKUP_VERSION,
            created_at: now_ms(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(manifest.created_at / 1000);
        header.set_cksum();
        builder.append_data(&mut header, BACKUP_MANIFEST_FILE, manifest_bytes.as_slice())?;
        builder.append_dir_all(".", source)?;
        let tar_file = builder.into_inner()?;
        drop(tar_file);

        let mut input = std::fs::File::open(&temp_tar)?;
        let output = create_private_file(&temp_archive)?;
        let encryptor = age_encryptor(passphrase);
        let mut writer = encryptor.wrap_output(output)?;
        std::io::copy(&mut input, &mut writer)?;
        let output = writer.finish()?;
        drop(output);
        std::fs::rename(&temp_archive, destination)?;
        Ok(())
    })();

    let _ = std::fs::remove_file(&temp_tar);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_archive);
    }
    result
}

/// Decrypt and import a full-directory archive into an empty data directory.
///
/// Extraction happens into a sibling temporary directory, the manifest and
/// required files are verified, and only then is the directory renamed into
/// place. A wrong passphrase, tampered archive, bad manifest, or partially
/// extracted crash leaves the destination untouched.
pub fn import_data_dir(
    destination: &Path,
    source_file: &Path,
    passphrase: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !passphrase.is_empty(),
        "backup passphrase must not be empty"
    );
    anyhow::ensure!(
        source_file.is_file(),
        "backup source is not a file: {}",
        source_file.display()
    );
    anyhow::ensure!(
        !destination.exists()
            || std::fs::read_dir(destination)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
        "restore target must not exist or must be empty: {}",
        destination.display()
    );
    anyhow::ensure!(
        !destination.starts_with(source_file),
        "restore target must not be inside the backup source"
    );
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let temp_tar = temp_sibling(destination, "tar");
    let temp_dir = temp_sibling(destination, "dir");
    let result = (|| -> anyhow::Result<()> {
        decrypt_to_file(source_file, &temp_tar, passphrase)?;

        std::fs::create_dir_all(&temp_dir)?;
        let mut archive = tar::Archive::new(std::fs::File::open(&temp_tar)?);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            validate_archive_path(&path)?;
            let entry_type = entry.header().entry_type();
            anyhow::ensure!(
                entry_type.is_file() || entry_type.is_dir(),
                "backup contains unsupported entry type for {}",
                path.display()
            );
            entry.unpack_in(&temp_dir)?;
        }

        let manifest_path = temp_dir.join(BACKUP_MANIFEST_FILE);
        let manifest_bytes = std::fs::read(&manifest_path)
            .with_context(|| format!("backup is missing {}", manifest_path.display()))?;
        let manifest: BackupManifest =
            serde_json::from_slice(&manifest_bytes).context("backup manifest is not valid JSON")?;
        anyhow::ensure!(
            manifest.format == BACKUP_FORMAT,
            "unsupported backup format: {}",
            manifest.format
        );
        anyhow::ensure!(
            manifest.version == BACKUP_VERSION,
            "unsupported backup version {}; this build understands version {}",
            manifest.version,
            BACKUP_VERSION
        );
        anyhow::ensure!(
            temp_dir.join("mnemonic").is_file(),
            "backup has no encrypted mnemonic"
        );
        anyhow::ensure!(
            temp_dir.join("fedimint.rocksdb").is_dir(),
            "backup has no fedimint.rocksdb"
        );
        anyhow::ensure!(
            temp_dir.join(BDK_DB_FILE).is_file(),
            "backup has no {BDK_DB_FILE}"
        );
        std::fs::remove_file(&manifest_path)?;

        if destination.exists() {
            std::fs::remove_dir(destination)?;
        }
        std::fs::rename(&temp_dir, destination)?;
        Ok(())
    })();

    let _ = std::fs::remove_file(&temp_tar);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    result
}

fn decrypt_to_file(source: &Path, destination: &Path, passphrase: &str) -> anyhow::Result<()> {
    let input = BufReader::new(std::fs::File::open(source)?);
    let decryptor = age::Decryptor::new_buffered(input)
        .map_err(|err| anyhow::anyhow!("not an age-encrypted backup: {err}"))?;
    let mut identity =
        age::scrypt::Identity::new(age::secrecy::SecretString::from(passphrase.to_owned()));
    identity.set_max_work_factor(22);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|err| {
            anyhow::anyhow!("could not decrypt backup (wrong passphrase or corrupt file): {err}")
        })?;
    let mut output = create_private_file(destination)?;
    std::io::copy(&mut reader, &mut output)?;
    output.flush()?;
    Ok(())
}

fn age_encryptor(passphrase: &str) -> age::Encryptor {
    age::Encryptor::with_user_passphrase(age::secrecy::SecretString::from(passphrase.to_owned()))
}

fn create_private_file(path: &Path) -> anyhow::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn validate_archive_path(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!path.is_absolute(), "backup contains an absolute path");
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => anyhow::bail!("backup contains an unsafe path: {}", path.display()),
        }
    }
    Ok(())
}

fn temp_sibling(path: &Path, tag: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_owned());
    path.with_file_name(format!(".{name}.{tag}-{}", uuid::Uuid::now_v7()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("berlin-wallet-backup-{}", uuid::Uuid::now_v7()))
    }

    fn make_source(dir: &Path, passphrase: &str) -> String {
        std::fs::create_dir_all(dir.join("fedimint.rocksdb")).unwrap();
        std::fs::write(dir.join("fedimint.rocksdb/CURRENT"), b"rocks").unwrap();
        std::fs::write(dir.join("bdk.sqlite"), b"sqlite").unwrap();
        std::fs::write(dir.join("bdk.sqlite-wal"), b"wal").unwrap();
        std::fs::write(dir.join("bdk.sqlite-shm"), b"shm").unwrap();
        std::fs::write(dir.join("chain.json"), b"{\"version\":1}").unwrap();
        let mnemonic =
            seed::load_or_create_mnemonic(&dir.join("mnemonic"), passphrase).expect("mnemonic");
        mnemonic.to_string()
    }

    #[test]
    fn encrypted_backup_round_trips_sidecars_and_chain_source() {
        let base = temp_dir();
        let source = base.join("source");
        let archive = base.join("backup.age");
        let imported = base.join("imported");
        let phrase = make_source(&source, "wallet pass");

        backup_data_dir(&source, &archive, "backup pass").expect("backup");
        let bytes = std::fs::read(&archive).expect("archive");
        assert!(
            !bytes
                .windows(phrase.len())
                .any(|window| window == phrase.as_bytes())
        );
        import_data_dir(&imported, &archive, "backup pass").expect("import");

        assert!(imported.join("fedimint.rocksdb/CURRENT").is_file());
        assert!(imported.join("bdk.sqlite-wal").is_file());
        assert!(imported.join("bdk.sqlite-shm").is_file());
        assert!(imported.join("chain.json").is_file());
        let imported_mnemonic =
            seed::load_or_create_mnemonic(&imported.join("mnemonic"), "wallet pass")
                .expect("imported mnemonic");
        assert_eq!(imported_mnemonic.to_string(), phrase);
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn wrong_passphrase_and_tampering_leave_destination_untouched() {
        let base = temp_dir();
        let source = base.join("source");
        let archive = base.join("backup.age");
        make_source(&source, "wallet pass");
        backup_data_dir(&source, &archive, "backup pass").expect("backup");

        let wrong = base.join("wrong");
        assert!(import_data_dir(&wrong, &archive, "bad pass").is_err());
        assert!(!wrong.exists());

        let mut bytes = std::fs::read(&archive).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&archive, bytes).unwrap();
        let tampered = base.join("tampered");
        assert!(import_data_dir(&tampered, &archive, "backup pass").is_err());
        assert!(!tampered.exists());
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn import_requires_an_empty_target_and_ignores_stale_temporary_dirs() {
        let base = temp_dir();
        let source = base.join("source");
        let archive = base.join("backup.age");
        make_source(&source, "wallet pass");
        backup_data_dir(&source, &archive, "backup pass").expect("backup");

        let nonempty = base.join("nonempty");
        std::fs::create_dir_all(&nonempty).unwrap();
        std::fs::write(nonempty.join("keep"), b"keep").unwrap();
        assert!(import_data_dir(&nonempty, &archive, "backup pass").is_err());

        let stale = base.join(format!(".imported.dir-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&stale).unwrap();
        let imported = base.join("imported");
        import_data_dir(&imported, &archive, "backup pass").expect("import");
        assert!(imported.join("mnemonic").is_file());
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn backup_refuses_a_legacy_plaintext_mnemonic() {
        let base = temp_dir();
        let source = base.join("source");
        std::fs::create_dir_all(source.join("fedimint.rocksdb")).unwrap();
        std::fs::write(source.join("fedimint.rocksdb/CURRENT"), b"rocks").unwrap();
        std::fs::write(source.join("bdk.sqlite"), b"sqlite").unwrap();
        std::fs::write(source.join("mnemonic"), b"abandon abandon").unwrap();
        let archive = base.join("backup.age");

        let err = backup_data_dir(&source, &archive, "backup pass")
            .expect_err("plaintext mnemonic must not be backed up");
        assert!(err.to_string().contains("legacy plaintext"), "{err:#}");
        assert!(!archive.exists());
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn backup_refuses_a_live_database_and_ignores_stale_archive_temps() {
        let base = temp_dir();
        let source = base.join("source");
        let archive = base.join("backup.age");
        make_source(&source, "wallet pass");
        let stale = temp_sibling(&archive, "age");
        std::fs::write(&stale, b"stale").unwrap();

        let lock_path = source.join("fedimint.db.lock");
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)
            .unwrap();
        let _lock = fs_lock::FileLock::new_try_exclusive(lock_file).unwrap();
        assert!(backup_data_dir(&source, &archive, "backup pass").is_err());
        assert!(!archive.exists());
        assert!(
            stale.exists(),
            "stale temp files are ignored, not overwritten"
        );
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn backup_does_not_overwrite_an_existing_archive() {
        let base = temp_dir();
        let source = base.join("source");
        let archive = base.join("backup.age");
        make_source(&source, "wallet pass");
        backup_data_dir(&source, &archive, "backup pass").expect("first backup");
        let first = std::fs::read(&archive).unwrap();
        assert!(backup_data_dir(&source, &archive, "backup pass").is_err());
        assert_eq!(std::fs::read(&archive).unwrap(), first);
        std::fs::remove_dir_all(base).ok();
    }
}
