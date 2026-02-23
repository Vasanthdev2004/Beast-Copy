use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, error};

use crate::types::{OrderIntent, OrderResult, OrderStatus};
use crate::config::AppConfig;

pub struct ClobExecutor {
    intent_rx: mpsc::Receiver<OrderIntent>,
    result_tx: mpsc::Sender<OrderResult>,
    config: Arc<RwLock<AppConfig>>,
}

impl ClobExecutor {
    pub fn new(
        intent_rx: mpsc::Receiver<OrderIntent>,
        result_tx: mpsc::Sender<OrderResult>,
        config: Arc<RwLock<AppConfig>>,
    ) -> Self {
        Self {
            intent_rx,
            result_tx,
            config,
        }
    }

    pub async fn run(mut self) {
        info!("ClobExecutor started");
        
        while let Some(intent) = self.intent_rx.recv().await {
            self.execute_order(intent).await;
        }
    }

    async fn execute_order(&self, intent: OrderIntent) {
        let config = self.config.read().await;
        
        if config.copy.preview_mode {
            info!("[PREVIEW] Executing order: {:?}", intent);
            
            let result = OrderResult {
                order_id: format!("preview-{}", chrono::Utc::now().timestamp_millis()),
                status: OrderStatus::Filled,
                tx_hash: None, // No real tx hash in preview
                filled_at: intent.price,
                timestamp: chrono::Utc::now().timestamp_millis() as u64,
            };
            
            if let Err(e) = self.result_tx.send(result).await {
                error!("Failed to route preview execution result: {}", e);
            }
            return;
        }

        info!("Sending actual order to Polymarket CLOB: {:?}", intent);
        // rs-clob-client integration: order framing, EIP-712 signing, submitting
        // Result would then be pushed to result_tx
    }
}
