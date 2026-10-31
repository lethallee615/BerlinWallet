//! Restore policy and user-facing warnings.
//!
//! fedimint-client 0.12.0 recovery rebuilds settled module state but leaves the
//! local operation log empty. The application journal is local too, so a fresh
//! mnemonic-only restore has no deposit/withdrawal history and cannot resume an
//! in-flight operation. This module keeps that contract in one place so the CLI
//! and FFI cannot drift into promising more than 0.12.0 can do.

/// Exact text shown when restored balances are available but history is not.
pub const HISTORY_UNAVAILABLE_WARNING: &str =
    "History unavailable after restore — balances recovered";

/// Exact text warning that an earlier funded deposit may be stranded.
pub const STRANDED_DEPOSIT_WARNING: &str = "A previous deposit may still need to be claimed; keep the original client data. \
     A new deposit request does not recover it.";

/// Exact text describing the supported recovery artifact.
pub const RECOVERY_BACKUP_WARNING: &str =
    "Recovery requires your mnemonic plus the original client data backup.";

/// What a fresh restore recovered, and what it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSummary {
    /// The recovered client's operation log is empty.
    pub operation_log_empty: bool,
    /// The UI must say history is unavailable rather than "no transactions".
    pub history_unavailable: bool,
    /// Application journal rows in the fresh data directory.
    pub deposit_count: u32,
    /// Application journal rows in the fresh data directory.
    pub withdrawal_count: u32,
    /// Warnings the CLI/FFI must display for this result.
    pub warnings: Vec<&'static str>,
}

impl RestoreSummary {
    pub fn new(operation_log_empty: bool, deposit_count: usize, withdrawal_count: usize) -> Self {
        let history_unavailable = operation_log_empty;
        let warnings = if history_unavailable {
            vec![
                HISTORY_UNAVAILABLE_WARNING,
                STRANDED_DEPOSIT_WARNING,
                RECOVERY_BACKUP_WARNING,
            ]
        } else {
            Vec::new()
        };
        Self {
            operation_log_empty,
            history_unavailable,
            deposit_count: u32::try_from(deposit_count).unwrap_or(u32::MAX),
            withdrawal_count: u32::try_from(withdrawal_count).unwrap_or(u32::MAX),
            warnings,
        }
    }

    /// A fresh mnemonic restore must be balances-only: no operation log and no
    /// application history. A future upstream change that starts resuming
    /// operations must update this policy and its warning text deliberately.
    pub fn is_balances_only(&self) -> bool {
        self.history_unavailable && self.deposit_count == 0 && self.withdrawal_count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_restore_is_balances_only_and_warns() {
        let summary = RestoreSummary::new(true, 0, 0);
        assert!(summary.operation_log_empty);
        assert!(summary.history_unavailable);
        assert_eq!(summary.deposit_count, 0);
        assert_eq!(summary.withdrawal_count, 0);
        assert!(summary.is_balances_only());
        assert_eq!(
            summary.warnings,
            vec![
                HISTORY_UNAVAILABLE_WARNING,
                STRANDED_DEPOSIT_WARNING,
                RECOVERY_BACKUP_WARNING,
            ]
        );
    }

    #[test]
    fn retained_history_does_not_claim_history_is_unavailable() {
        let summary = RestoreSummary::new(false, 1, 1);
        assert!(!summary.operation_log_empty);
        assert!(!summary.history_unavailable);
        assert!(!summary.is_balances_only());
        assert!(summary.warnings.is_empty());
    }

    #[test]
    fn stranded_deposit_warning_does_not_promise_recovery() {
        assert!(STRANDED_DEPOSIT_WARNING.contains("keep the original client data"));
        assert!(STRANDED_DEPOSIT_WARNING.contains("A new deposit request does not recover it."));
    }

    #[test]
    fn record_counts_saturate_instead_of_wrapping() {
        let summary = RestoreSummary::new(true, usize::MAX, usize::MAX);
        assert_eq!(summary.deposit_count, u32::MAX);
        assert_eq!(summary.withdrawal_count, u32::MAX);
    }
}
