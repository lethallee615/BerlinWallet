//! Berlin Wallet core — type-checking scaffold of the walletv2 + BDK peg flows.
//!
//! Real types and state transitions live in [`journal`]; the flow functions
//! in [`deposit`]/[`withdrawal`] use the exact API sequences from PLAN.md §4.
//! Operation-log lookups that the plan glosses over are explicit stubs (see
//! COMPILE.md).

pub mod bdk;
pub mod client;
pub mod deposit;
pub mod error;
pub mod journal;
mod journal_db;
pub mod ops;
pub mod sync;
pub mod wire;
pub mod withdrawal;

pub use error::{Classify, ErrorClass, PegError, classify_error};
pub use journal::{
    AdoptedClaim, ApplyResult, BindResult, DepositOrigin, DepositRow, DepositState, Journal,
    JournalBackend, WithdrawalRow, WithdrawalState,
};
pub use ops::{DepositId, WithdrawalId};
pub use wire::FeeQuoteWire;
