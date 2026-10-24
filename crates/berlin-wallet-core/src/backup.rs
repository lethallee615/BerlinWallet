//! Full data-directory backup/import helpers.
//!
//! The artifact is the entire directory: mnemonic, `chain.json` (the selected
//! chain source, when the data directory has recorded one), BDK SQLite
//! (including `-wal`/`-shm`), and the fedimint RocksDB directory. `chain.json`
//! is not a precondition — it is copied only because [`copy_dir`] takes every
//! entry — but dropping it would silently reset the chain backend on restore.
//! A hand-picked file list can lose it, WAL commits, or RocksDB sidecars.

use std::path::Path;

use anyhow::Context as _;

use crate::bdk::BDK_DB_FILE;

/// Copy the complete data directory while holding the fedimint DB lock.
pub fn backup_data_dir(source: &Path, destination: &Path) -> anyhow::Result<()> {
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
        "backup destination must not exist or must be empty: {}",
        destination.display()
    );
    anyhow::ensure!(
        !destination.starts_with(source),
        "backup destination must not be inside the data directory"
    );
    std::fs::create_dir_all(destination)?;

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

    copy_dir(source, destination)?;
    Ok(())
}

/// Import a full directory backup into an empty data directory.
pub fn import_data_dir(destination: &Path, source: &Path) -> anyhow::Result<()> {
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
    anyhow::ensure!(
        !destination.starts_with(source),
        "restore target must not be inside the backup source"
    );
    std::fs::create_dir_all(destination)?;
    copy_dir(source, destination)?;
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
