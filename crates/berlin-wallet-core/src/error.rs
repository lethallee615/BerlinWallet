use fedimint_client::module::transaction::FeeQuote;
use fedimint_walletv2_client::{ReceiveError, SendError};
use thiserror::Error;

/// How a wallet/federation error should be treated by the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transient,
    Permanent,
}

/// The plan's `classify_error(&err)` assumed one concrete parameter type; the
/// real call sites carry `SendError`, `ReceiveError` and `anyhow::Error`.
/// A small trait is the type-safe way to cover all three.
pub trait Classify {
    fn class(&self) -> ErrorClass;
}

impl Classify for SendError {
    fn class(&self) -> ErrorClass {
        match self {
            SendError::WrongNetwork
            | SendError::DustValue
            | SendError::UnsupportedAddress
            | SendError::InsufficientFunds => ErrorClass::Permanent,
            SendError::NoConsensusFeerateAvailable | SendError::FederationError(_) => {
                ErrorClass::Transient
            }
        }
    }
}

impl Classify for ReceiveError {
    fn class(&self) -> ErrorClass {
        match self {
            ReceiveError::NoConsensusFeerateAvailable | ReceiveError::FederationError(_) => {
                ErrorClass::Transient
            }
        }
    }
}

impl Classify for anyhow::Error {
    fn class(&self) -> ErrorClass {
        if let Some(err) = self.downcast_ref::<SendError>() {
            return err.class();
        }
        if let Some(err) = self.downcast_ref::<ReceiveError>() {
            return err.class();
        }
        // Conservative default: unknown errors are retried, never terminalised.
        ErrorClass::Transient
    }
}

pub fn classify_error<E: Classify + ?Sized>(err: &E) -> ErrorClass {
    err.class()
}

#[derive(Debug, Error)]
pub enum PegError {
    #[error("quote changed (quoted {quoted:?}, current {current:?})")]
    QuoteChanged {
        quoted: FeeQuote,
        current: FeeQuote,
    },

    #[error("reconciliation scan inconclusive; refusing to re-send")]
    InconclusiveScan,

    #[error("deposit not found: {0}")]
    DepositNotFound(String),

    #[error("withdrawal not found: {0}")]
    WithdrawalNotFound(String),

    #[error("withdrawal value {value} is below the federation dust limit {dust_limit}")]
    WithdrawalBelowDust {
        value: bitcoin::Amount,
        dust_limit: bitcoin::Amount,
    },

    #[error("deposit address not ready")]
    AddressNotReady,

    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}
