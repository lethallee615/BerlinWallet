//! Berlin Wallet core — type-checking scaffold of the walletv2 + BDK peg flows.
//!
//! Real types and state transitions live in [`journal`]; the flow functions
//! in [`deposit`]/[`withdrawal`] use the exact API sequences from
//! `docs/PLAN.md` §4. Operation-log lookups that the plan glosses over are
//! explicit stubs (see `docs/API-VERIFICATION.md`).

pub mod backup;
pub mod bdk;
pub mod client;
pub mod deposit;
pub mod error;
mod fault;
pub mod journal;
mod journal_db;
pub mod network;
pub mod ops;
pub mod seed;
pub mod sqlite;
pub mod sync;
pub mod wire;
pub mod withdrawal;

pub use client::{min_withdrawal_sat, open_rocksdb, open_sqlite};
pub use error::{Classify, ErrorClass, PegError, classify_error};
pub use journal::{
    AdoptedClaim, ApplyResult, BindResult, DepositOrigin, DepositRow, DepositState, Journal,
    JournalBackend, WithdrawalRow, WithdrawalState,
};
pub use network::{client_network, network_from_chain_id};
pub use ops::{DepositId, WithdrawalId};
pub use seed::{load_or_create_mnemonic, mnemonic_seed, root_secret_from_mnemonic};
pub use sqlite::SqliteDb;
pub use wire::FeeQuoteWire;
