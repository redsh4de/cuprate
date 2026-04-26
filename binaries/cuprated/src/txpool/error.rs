//! Error type for handling incoming transactions.

use cuprate_consensus::ExtendedConsensusError;
use cuprate_consensus_rules::ConsensusError;
use cuprate_txpool::TxPoolError;

use crate::txpool::relay_rules::RelayRuleError;

/// An error that can happen handling an incoming tx.
#[derive(Debug, thiserror::Error)]
pub enum IncomingTxError {
    #[error("Error parsing tx: {0}")]
    Parse(#[from] std::io::Error),
    #[error(transparent)]
    Consensus(#[from] ExtendedConsensusError),
    #[error("Duplicate tx in message")]
    DuplicateTransaction,
    #[error("Relay rule was broken: {0}")]
    RelayRule(RelayRuleError),
    /// An internal service error occurred.
    #[error(transparent)]
    Service(anyhow::Error),
}

impl From<tower::BoxError> for IncomingTxError {
    fn from(e: tower::BoxError) -> Self {
        Self::Service(anyhow::Error::from_boxed(e))
    }
}

impl From<TxPoolError> for IncomingTxError {
    fn from(e: TxPoolError) -> Self {
        Self::Service(e.into())
    }
}

impl From<ConsensusError> for IncomingTxError {
    fn from(e: ConsensusError) -> Self {
        Self::Consensus(e.into())
    }
}
