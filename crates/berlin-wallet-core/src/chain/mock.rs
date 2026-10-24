//! Test-only chain backend with canned answers.
//!
//! It exists so the reorg anchor check and the retry policy can be exercised
//! through the [`ChainBackend`] trait for every [`ChainKind`] without network
//! I/O, and so tests can assert that a rebuild happened (`full_scan_calls`)
//! rather than only that some error came back.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::{BlockHash, Transaction};

use super::{ChainBackend, ChainError, ChainKind, ChainTip, FullScanOptions};

/// A queued failure for [`MockBackend::tip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockFailure {
    Transient,
    Permanent,
}

/// A backend that returns canned chain data and counts what the wallet asked
/// for.
pub struct MockBackend {
    kind: ChainKind,
    tip: Mutex<ChainTip>,
    remote_at_local: Mutex<Option<BlockHash>>,
    tip_failures: Mutex<VecDeque<MockFailure>>,
    tip_calls: AtomicUsize,
    sync_calls: AtomicUsize,
    full_scan_calls: AtomicUsize,
    last_scan_options: Mutex<Option<FullScanOptions>>,
}

impl MockBackend {
    pub fn new(kind: ChainKind, tip: ChainTip, remote_at_local: Option<BlockHash>) -> Self {
        Self {
            kind,
            tip: Mutex::new(tip),
            remote_at_local: Mutex::new(remote_at_local),
            tip_failures: Mutex::new(VecDeque::new()),
            tip_calls: AtomicUsize::new(0),
            sync_calls: AtomicUsize::new(0),
            full_scan_calls: AtomicUsize::new(0),
            last_scan_options: Mutex::new(None),
        }
    }

    pub fn set_tip(&self, tip: ChainTip) {
        *self.tip.lock().expect("mock tip lock") = tip;
    }

    pub fn set_remote_at_local(&self, hash: Option<BlockHash>) {
        *self.remote_at_local.lock().expect("mock hash lock") = hash;
    }

    /// Queue failures returned by the next `tip()` calls.
    pub fn queue_tip_failures(&self, failures: impl IntoIterator<Item = MockFailure>) {
        self.tip_failures
            .lock()
            .expect("mock failure lock")
            .extend(failures);
    }

    pub fn tip_calls(&self) -> usize {
        self.tip_calls.load(Ordering::SeqCst)
    }

    pub fn sync_calls(&self) -> usize {
        self.sync_calls.load(Ordering::SeqCst)
    }

    pub fn full_scan_calls(&self) -> usize {
        self.full_scan_calls.load(Ordering::SeqCst)
    }

    pub fn last_scan_options(&self) -> Option<FullScanOptions> {
        *self.last_scan_options.lock().expect("mock options lock")
    }
}

#[async_trait]
impl ChainBackend for MockBackend {
    fn kind(&self) -> ChainKind {
        self.kind
    }

    fn describe(&self) -> String {
        format!("mock {}", self.kind)
    }

    async fn tip(&self) -> Result<ChainTip, ChainError> {
        self.tip_calls.fetch_add(1, Ordering::SeqCst);
        let failure = self
            .tip_failures
            .lock()
            .expect("mock failure lock")
            .pop_front();
        match failure {
            Some(MockFailure::Transient) => Err(ChainError::transient(
                self.kind,
                anyhow::anyhow!("mock transient failure"),
            )),
            Some(MockFailure::Permanent) => Err(ChainError::permanent(
                self.kind,
                anyhow::anyhow!("mock permanent failure"),
            )),
            None => Ok(*self.tip.lock().expect("mock tip lock")),
        }
    }

    async fn block_hash_at(&self, _height: u32) -> Result<Option<BlockHash>, ChainError> {
        Ok(*self.remote_at_local.lock().expect("mock hash lock"))
    }

    async fn broadcast(&self, _tx: &Transaction) -> Result<(), ChainError> {
        Ok(())
    }

    async fn sync(&self, _wallet: &mut Wallet) -> Result<(), ChainError> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn full_scan(
        &self,
        _wallet: &mut Wallet,
        options: FullScanOptions,
    ) -> Result<Vec<(KeychainKind, u32)>, ChainError> {
        self.full_scan_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_scan_options.lock().expect("mock options lock") = Some(options);
        Ok(Vec::new())
    }
}
