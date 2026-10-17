//! Minimal persistent `IRawDatabase` backend over SQLite.
//!
//! Fedimint does not publish a 0.12.0 RocksDB backend (`fedimint-rocksdb` stops
//! at 0.8.1) and there is no `fedimint-sqlite` crate. This backend uses the
//! already-pinned `rusqlite` and implements per-key optimistic concurrency:
//! every read/write key records its snapshot version, and commit succeeds only
//! if those versions are still current. Otherwise it returns
//! `DatabaseError::WriteConflict`, so the journal's retry wrapper can re-run the
//! transition.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use fedimint_core::db::{
    DatabaseError, DatabaseResult, IDatabaseTransactionOps, IDatabaseTransactionOpsCore,
    IRawDatabase, IRawDatabaseTransaction, PrefixStream,
};
use futures::stream;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

#[derive(Debug, Clone)]
pub struct SqliteDb {
    path: PathBuf,
}

impl SqliteDb {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Self { path };
        db.init_schema()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&db.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(db)
    }

    fn connect(&self) -> DatabaseResult<Connection> {
        Connection::open(&self.path).map_err(backend)
    }

    fn init_schema(&self) -> DatabaseResult<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv (
                 key BLOB PRIMARY KEY NOT NULL,
                 value BLOB NOT NULL,
                 version INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 version INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO meta (id, version) VALUES (1, 0);",
        )
        .map_err(backend)?;
        Ok(())
    }

    fn snapshot(&self) -> DatabaseResult<BTreeMap<Vec<u8>, (Vec<u8>, i64)>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare("SELECT key, value, version FROM kv")
            .map_err(backend)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    (row.get::<_, Vec<u8>>(1)?, row.get::<_, i64>(2)?),
                ))
            })
            .map_err(backend)?;
        let mut values = BTreeMap::new();
        for row in rows {
            let (key, value) = row.map_err(backend)?;
            values.insert(key, value);
        }
        Ok(values)
    }
}

fn backend(err: rusqlite::Error) -> DatabaseError {
    DatabaseError::DatabaseBackend(Box::new(err))
}

fn io_backend(err: std::io::Error) -> DatabaseError {
    DatabaseError::DatabaseBackend(Box::new(err))
}

#[async_trait]
impl IRawDatabase for SqliteDb {
    type Transaction<'a> = SqliteTransaction<'a>;

    async fn begin_transaction<'a>(&'a self) -> Self::Transaction<'a> {
        match self.snapshot() {
            Ok(snapshot) => SqliteTransaction {
                db: self,
                snapshot,
                reads: BTreeSet::new(),
                writes: BTreeMap::new(),
                error: None,
            },
            Err(error) => SqliteTransaction {
                db: self,
                snapshot: BTreeMap::new(),
                reads: BTreeSet::new(),
                writes: BTreeMap::new(),
                error: Some(error),
            },
        }
    }

    fn checkpoint(&self, backup_path: &Path) -> DatabaseResult<()> {
        std::fs::create_dir_all(backup_path).map_err(io_backend)?;
        std::fs::copy(&self.path, backup_path.join("fedimint.sqlite")).map_err(io_backend)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct SqliteTransaction<'a> {
    db: &'a SqliteDb,
    snapshot: BTreeMap<Vec<u8>, (Vec<u8>, i64)>,
    reads: BTreeSet<Vec<u8>>,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    error: Option<DatabaseError>,
}

impl SqliteTransaction<'_> {
    fn take_error(&mut self) -> Option<DatabaseError> {
        self.error.take()
    }

    fn combined(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let mut values: BTreeMap<Vec<u8>, Vec<u8>> = self
            .snapshot
            .iter()
            .map(|(key, (value, _))| (key.clone(), value.clone()))
            .collect();
        for (key, value) in &self.writes {
            match value {
                Some(value) => {
                    values.insert(key.clone(), value.clone());
                }
                None => {
                    values.remove(key);
                }
            }
        }
        values
    }

    fn rows_matching(&self, predicate: impl Fn(&[u8]) -> bool) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.combined()
            .into_iter()
            .filter(|(key, _)| predicate(key.as_slice()))
            .collect()
    }

    fn commit(self) -> DatabaseResult<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.writes.is_empty() {
            return Ok(());
        }

        let mut conn = self.db.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;

        // Check every exact read and every written key against the snapshot.
        // This catches read-check-write races (e.g. the open-deposit index)
        // without making disjoint prefixes conflict.
        let mut checked: BTreeSet<Vec<u8>> = self.reads;
        checked.extend(self.writes.keys().cloned());
        for key in checked {
            let current: Option<i64> = tx
                .query_row(
                    "SELECT version FROM kv WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(backend)?;
            let expected = self.snapshot.get(&key).map(|(_, version)| *version);
            if current != expected {
                return Err(DatabaseError::WriteConflict);
            }
        }

        let current_sequence: i64 = tx
            .query_row("SELECT version FROM meta WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map_err(backend)?;
        let new_version = current_sequence.saturating_add(1);

        for (key, value) in self.writes {
            match value {
                Some(value) => {
                    tx.execute(
                        "INSERT OR REPLACE INTO kv (key, value, version) VALUES (?1, ?2, ?3)",
                        params![key, value, new_version],
                    )
                    .map_err(backend)?;
                }
                None => {
                    tx.execute("DELETE FROM kv WHERE key = ?1", params![key])
                        .map_err(backend)?;
                }
            }
        }
        tx.execute(
            "UPDATE meta SET version = ?1 WHERE id = 1",
            params![new_version],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(())
    }
}

#[async_trait]
impl IDatabaseTransactionOpsCore for SqliteTransaction<'_> {
    async fn raw_insert_bytes(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> DatabaseResult<Option<Vec<u8>>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let old = self
            .writes
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.snapshot.get(key).map(|(value, _)| value.clone()));
        self.writes.insert(key.to_vec(), Some(value.to_vec()));
        Ok(old)
    }

    async fn raw_get_bytes(&mut self, key: &[u8]) -> DatabaseResult<Option<Vec<u8>>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        self.reads.insert(key.to_vec());
        Ok(self
            .writes
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.snapshot.get(key).map(|(value, _)| value.clone())))
    }

    async fn raw_remove_entry(&mut self, key: &[u8]) -> DatabaseResult<Option<Vec<u8>>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let old = self
            .writes
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.snapshot.get(key).map(|(value, _)| value.clone()));
        self.writes.insert(key.to_vec(), None);
        Ok(old)
    }

    async fn raw_find_by_prefix(&mut self, key_prefix: &[u8]) -> DatabaseResult<PrefixStream<'_>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let rows = self.rows_matching(|key| key.starts_with(key_prefix));
        self.reads.extend(rows.iter().map(|(key, _)| key.clone()));
        Ok(Box::pin(stream::iter(rows)))
    }

    async fn raw_find_by_prefix_sorted_descending(
        &mut self,
        key_prefix: &[u8],
    ) -> DatabaseResult<PrefixStream<'_>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let mut rows = self.rows_matching(|key| key.starts_with(key_prefix));
        self.reads.extend(rows.iter().map(|(key, _)| key.clone()));
        rows.reverse();
        Ok(Box::pin(stream::iter(rows)))
    }

    async fn raw_find_by_range(&mut self, range: Range<&[u8]>) -> DatabaseResult<PrefixStream<'_>> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let rows = self.rows_matching(|key| key >= range.start && key < range.end);
        self.reads.extend(rows.iter().map(|(key, _)| key.clone()));
        Ok(Box::pin(stream::iter(rows)))
    }

    async fn raw_remove_by_prefix(&mut self, key_prefix: &[u8]) -> DatabaseResult<()> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let keys: Vec<Vec<u8>> = self
            .combined()
            .into_keys()
            .filter(|key| key.starts_with(key_prefix))
            .collect();
        for key in keys {
            self.writes.insert(key, None);
        }
        Ok(())
    }
}

impl IDatabaseTransactionOps for SqliteTransaction<'_> {}

#[async_trait]
impl IRawDatabaseTransaction for SqliteTransaction<'_> {
    async fn commit_tx(self) -> DatabaseResult<()> {
        self.commit()
    }
}

impl fmt::Display for SqliteDb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SqliteDb({})", self.path.display())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use fedimint_core::config::FederationId;
    use fedimint_core::db::{Database, DatabaseError};
    use fedimint_core::module::registry::ModuleDecoderRegistry;
    use fedimint_eventlog::EventLogId;

    use super::*;
    use crate::journal::{DepositRow, Journal};
    use crate::ops::DepositId;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "berlin-wallet-{label}-{}.sqlite",
            uuid::Uuid::now_v7()
        ))
    }

    #[tokio::test]
    async fn journal_rows_survive_reopen() {
        let path = temp_path("persist");
        let federation_id = FederationId::dummy();
        let id = DepositId::new();
        let amount = bitcoin::Amount::from_sat(50_000);

        {
            let db = Database::new(
                SqliteDb::open(&path).expect("open db"),
                ModuleDecoderRegistry::default(),
            );
            let journal = Journal::new(db, vec![0x00]);
            let row =
                DepositRow::requested(id.clone(), federation_id, EventLogId::LOG_START, amount);
            journal.insert_deposit(&row).await.expect("insert");
        }

        let db = Database::new(
            SqliteDb::open(&path).expect("reopen db"),
            ModuleDecoderRegistry::default(),
        );
        let journal = Journal::new(db, vec![0x00]);
        let row = journal
            .deposit(&id)
            .await
            .expect("read")
            .expect("row must survive reopen");
        assert_eq!(row.id, id);
        assert_eq!(row.amount, amount);
        assert_eq!(row.federation_id, federation_id);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn write_conflict_is_reported() {
        let path = temp_path("conflict");
        let db = SqliteDb::open(&path).expect("open db");

        let mut first = db.begin_transaction().await;
        let mut second = db.begin_transaction().await;
        first
            .raw_insert_bytes(b"conflict-key", b"first")
            .await
            .expect("first write");
        second
            .raw_insert_bytes(b"conflict-key", b"second")
            .await
            .expect("second write");
        first.commit_tx().await.expect("first commit");
        assert!(matches!(
            second.commit_tx().await,
            Err(DatabaseError::WriteConflict)
        ));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn raw_rows_survive_reopen() {
        let path = temp_path("raw");
        {
            let db = Database::new(
                SqliteDb::open(&path).expect("open db"),
                ModuleDecoderRegistry::default(),
            );
            let mut dbtx = db.begin_transaction().await;
            dbtx.raw_insert_bytes(b"config-key", b"config-value")
                .await
                .expect("write");
            dbtx.commit_tx_result().await.expect("commit");
        }

        let db = Database::new(
            SqliteDb::open(&path).expect("reopen db"),
            ModuleDecoderRegistry::default(),
        );
        let mut dbtx = db.begin_transaction().await;
        assert_eq!(
            dbtx.raw_get_bytes(b"config-key")
                .await
                .expect("read")
                .as_deref(),
            Some(b"config-value".as_slice())
        );

        let _ = std::fs::remove_file(path);
    }
}
