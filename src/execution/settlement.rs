use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::types::{OrderResult, OrderStatus};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;


pub struct SettlementMonitor {
    result_rx: mpsc::Receiver<OrderResult>,
    _position_tracker: Arc<PositionTracker>,
    config: Arc<RwLock<AppConfig>>,
}

impl SettlementMonitor {
    pub fn new(
        result_rx: mpsc::Receiver<OrderResult>,
        position_tracker: Arc<PositionTracker>,
        config: Arc<RwLock<AppConfig>>,
    ) -> Self {
        Self {
            result_rx,
            _position_tracker: position_tracker,
            config,
        }
    }

    pub async fn run(mut self) {
        info!("SettlementMonitor started");

        while let Some(result) = self.result_rx.recv().await {
            self.process_result(result).await;
        }
    }

    async fn process_result(&self, result: OrderResult) {
        let config = self.config.read().await;

        if config.copy.preview_mode {
            info!("[PREVIEW] Order 'settled' immediately: {}", result.order_id);
            return;
        }

        match result.status {
            OrderStatus::Filled => {
                if let Some(tx_hash) = result.tx_hash {
                    info!("Waiting for on-chain confirmation for tx: {:?}", tx_hash);
                    // Future: check Polygon RPC for receipt
                }
            }
            OrderStatus::Rejected => warn!("Order rejected: {}", result.order_id),
            OrderStatus::Timeout => warn!("Order timed out: {}", result.order_id),
        }
    }
}
