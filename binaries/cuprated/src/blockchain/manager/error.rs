//! Error types for the blockchain manager.

use cuprate_blockchain::BlockchainError;
use cuprate_consensus::ExtendedConsensusError;
use cuprate_consensus_rules::ConsensusError;
use cuprate_txpool::TxPoolError;

/// An error returned from blockchain manager operations.
///
/// `Validation` is peer-related; `Service` is an internal service failure.
#[derive(Debug, thiserror::Error)]
pub enum BlockchainManagerError {
    #[error(transparent)]
    Validation(anyhow::Error),
    #[error(transparent)]
    Service(anyhow::Error),
}

impl From<tower::BoxError> for BlockchainManagerError {
    fn from(e: tower::BoxError) -> Self {
        Self::Service(anyhow::Error::from_boxed(e))
    }
}

impl From<BlockchainError> for BlockchainManagerError {
    fn from(e: BlockchainError) -> Self {
        Self::Service(e.into())
    }
}

impl From<ExtendedConsensusError> for BlockchainManagerError {
    fn from(e: ExtendedConsensusError) -> Self {
        Self::Validation(e.into())
    }
}

impl From<ConsensusError> for BlockchainManagerError {
    fn from(e: ConsensusError) -> Self {
        Self::Validation(e.into())
    }
}

/// An error that can be returned from [`BlockchainManagerHandle::handle_incoming_block`](crate::blockchain::interface::BlockchainManagerHandle::handle_incoming_block).
#[derive(Debug, thiserror::Error)]
pub enum IncomingBlockError {
    /// Some transactions in the block were unknown.
    ///
    /// The inner values are the block hash and the indexes of the missing txs in the block.
    #[error("Unknown transactions in block.")]
    UnknownTransactions([u8; 32], Vec<usize>),
    /// We are missing the block's parent.
    #[error("The block has an unknown parent.")]
    Orphan,
    /// The block was invalid.
    #[error(transparent)]
    Validation(anyhow::Error),
    /// An internal service error occurred.
    #[error(transparent)]
    Service(anyhow::Error),
    /// The blockchain manager command channel is closed.
    #[error("The blockchain manager command channel is closed.")]
    ChannelClosed,
}

impl From<BlockchainManagerError> for IncomingBlockError {
    fn from(e: BlockchainManagerError) -> Self {
        match e {
            BlockchainManagerError::Validation(e) => Self::Validation(e),
            BlockchainManagerError::Service(e) => Self::Service(e),
        }
    }
}

impl From<BlockchainError> for IncomingBlockError {
    fn from(e: BlockchainError) -> Self {
        Self::Service(e.into())
    }
}

impl From<TxPoolError> for IncomingBlockError {
    fn from(e: TxPoolError) -> Self {
        Self::Service(e.into())
    }
}

impl From<ConsensusError> for IncomingBlockError {
    fn from(e: ConsensusError) -> Self {
        Self::Validation(e.into())
    }
}
