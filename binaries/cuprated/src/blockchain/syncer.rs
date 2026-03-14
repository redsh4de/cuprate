use std::{cmp::Ordering, sync::Arc};

use futures::StreamExt;
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use tower::{Service, ServiceExt};
use tracing::instrument;

use cuprate_consensus::{BlockChainContextRequest, BlockChainContextResponse, BlockchainContext};
use cuprate_consensus_context::BlockchainContextService;
use cuprate_p2p::{
    block_downloader::{BlockBatch, BlockDownloaderConfig, ChainSvcRequest, ChainSvcResponse},
    NetworkInterface, PeerSetRequest, PeerSetResponse,
};
use cuprate_p2p_core::{ClearNet, NetworkZone, SyncEvent};

use super::interface::{is_block_being_handled, wait_for_inflight_fluffy_blocks};

/// An error returned from the [`Syncer`].
#[derive(Debug, thiserror::Error)]
pub enum SyncerError {
    #[error("Incoming block channel closed.")]
    IncomingBlockChannelClosed,
    #[error("Sync event channel closed.")]
    SyncEventChannelClosed,
    #[error("One of our services returned an error: {0}.")]
    ServiceError(#[from] tower::BoxError),
}

/// The syncer that makes sure we are fully synchronised with our connected peers.
pub struct Syncer {
    /// The blockchain context service.
    pub(super) context_svc: BlockchainContextService,
    /// The clearnet P2P network interface.
    pub(super) clearnet_interface: NetworkInterface<ClearNet>,
    /// Notified once when we first become synchronised with the network.
    pub(super) synced_notify: Arc<Notify>,
    /// Receives P2P sync signals.
    pub(super) sync_event_rx: mpsc::Receiver<SyncEvent>,
    /// Whether we have declared ourselves synchronised with the network.
    pub(super) synced: bool,
}

impl Syncer {
    /// The main syncer task.
    #[instrument(level = "debug", skip_all)]
    #[expect(clippy::significant_drop_tightening)]
    pub async fn run<CN>(
        mut self,
        our_chain: CN,
        incoming_block_batch_tx: mpsc::Sender<(BlockBatch, Arc<OwnedSemaphorePermit>)>,
        stop_current_block_downloader: Arc<Notify>,
        block_downloader_config: BlockDownloaderConfig,
    ) -> Result<(), SyncerError>
    where
        CN: Service<
                ChainSvcRequest<ClearNet>,
                Response = ChainSvcResponse<ClearNet>,
                Error = tower::BoxError,
            > + Clone
            + Send
            + 'static,
        CN::Future: Send + 'static,
    {
        let semaphore = Arc::new(Semaphore::new(1));
        let mut sync_permit = Arc::new(Arc::clone(&semaphore).acquire_owned().await.unwrap());

        tracing::info!("Starting blockchain syncer");

        loop {
            self.check_and_park().await?;

            let mut block_batch_stream = self
                .clearnet_interface
                .block_downloader(our_chain.clone(), block_downloader_config);

            loop {
                tokio::select! {
                    () = stop_current_block_downloader.notified() => {
                        tracing::info!("Received stop signal, stopping block downloader");

                        drop(sync_permit);
                        sync_permit = Arc::new(Arc::clone(&semaphore).acquire_owned().await.unwrap());

                        break;
                    }
                    batch = block_batch_stream.next() => {
                        let Some(batch) = batch else {
                            // Wait for all references to the permit have been dropped (which means all blocks in the queue
                            // have been handled before checking if we are synced.
                            drop(sync_permit);
                            sync_permit = Arc::new(Arc::clone(&semaphore).acquire_owned().await.unwrap());
                            break;
                        };

                        tracing::debug!("Got batch, len: {}", batch.blocks.len());
                        if incoming_block_batch_tx.send((batch, Arc::clone(&sync_permit))).await.is_err() {
                            return Err(SyncerError::IncomingBlockChannelClosed);
                        }
                    }
                }
            }
        }
    }

    /// Checks our sync status and parks until we are behind peers.
    async fn check_and_park(&mut self) -> Result<(), SyncerError> {
        loop {
            tracing::trace!("Checking connected peers to see if we are behind.");
            let status = check_sync_status(
                self.context_svc.blockchain_context(),
                &mut self.clearnet_interface,
            )
            .await?;

            match status {
                SyncStatus::BehindPeers => {
                    // Wait for in-flight fluffy blocks - they may catch us up.
                    if self.synced && wait_for_inflight_fluffy_blocks().await {
                        tracing::debug!("Fluffy blocks finished, parking until next sync event.");
                        // Fall through to wait_for_sync_signal below.
                    } else {
                        tracing::debug!("Starting block downloader");
                        return Ok(());
                    }
                }
                SyncStatus::Synced => {
                    if !self.synced {
                        tracing::info!("Synchronised with the network.");
                        self.synced = true;
                        self.synced_notify.notify_one();
                    }
                }
                SyncStatus::AheadOfPeers => {}
                SyncStatus::NoPeers => {
                    tracing::debug!("Waiting for peers to connect.");
                    self.synced = false;
                }
            }

            tracing::debug!("Parking syncer.");
            self.wait_for_sync_signal().await?;
        }
    }

    /// Waits for a signal that syncing may be needed.
    async fn wait_for_sync_signal(&mut self) -> Result<(), SyncerError> {
        loop {
            let event = self
                .sync_event_rx
                .recv()
                .await
                .ok_or(SyncerError::SyncEventChannelClosed)?;

            match event {
                SyncEvent::Wake => return Ok(()),
                SyncEvent::NewState(peer_csd) => {
                    if !is_block_being_handled(&peer_csd.top_id)
                        && (!self.synced
                            || peer_csd.cumulative_difficulty()
                                > self.context_svc.blockchain_context().cumulative_difficulty)
                    {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum SyncStatus {
    NoPeers,
    BehindPeers,
    Synced,
    AheadOfPeers,
}

/// Checks if we are behind the connected peers.
async fn check_sync_status(
    blockchain_context: &BlockchainContext,
    clearnet_interface: &mut NetworkInterface<ClearNet>,
) -> Result<SyncStatus, tower::BoxError> {
    let PeerSetResponse::MostPoWSeen {
        cumulative_difficulty,
        ..
    } = clearnet_interface
        .peer_set()
        .ready()
        .await?
        .call(PeerSetRequest::MostPoWSeen)
        .await?
    else {
        unreachable!();
    };

    if cumulative_difficulty == 0 {
        return Ok(SyncStatus::NoPeers);
    }

    Ok(
        match cumulative_difficulty.cmp(&blockchain_context.cumulative_difficulty) {
            Ordering::Greater => SyncStatus::BehindPeers,
            Ordering::Less => SyncStatus::AheadOfPeers,
            Ordering::Equal => SyncStatus::Synced,
        },
    )
}
