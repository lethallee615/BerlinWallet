use serde::{Deserialize, Serialize};

/// Client-generated idempotency key for one deposit request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DepositId(pub String);

/// Client-generated idempotency key for one withdrawal request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WithdrawalId(pub String);

impl DepositId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }
}

impl WithdrawalId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }
}

impl Default for DepositId {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for WithdrawalId {
    fn default() -> Self {
        Self::new()
    }
}
