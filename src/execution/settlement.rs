use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

use crate::types::{OrderResult, OrderStatus, Position, Side};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;
use std::sync::atomic::{AtomicUsize, Ordering};



pub struct SettlementMonitor {
    result_rx: mpsc::Receiver<OrderResult>,
    position_tracker: Arc<PositionTracker>,
    config: Arc<RwLock<AppConfig>>,
    consecutive_losses: Arc<AtomicUsize>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
}

impl SettlementMonitor {
    pub fn new(
        result_rx: mpsc::Receiver<OrderResult>,
        position_tracker: Arc<PositionTracker>,
        config: Arc<RwLock<AppConfig>>,
        consecutive_losses: Arc<AtomicUsize>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
    ) -> Self {
        Self {
            result_rx,
            position_tracker,
            config,
            consecutive_losses,
            log_tx,
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
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "SETTLE".to_string(),
                message: format!("PAPER Settled: {}", result.order_id),
            });
            return;
        }

        match result.status {
            OrderStatus::Filled => {
                if let Some(tx_hash) = result.tx_hash {
                    info!("Waiting for on-chain confirmation for tx: {:?}", tx_hash);
                    
                    // The future listener runs as an independent WS loop, but for now we manually accept
                    self.consecutive_losses.store(0, Ordering::SeqCst);
                    
                    // Insert confirmed position via position_tracker
                    let position = Position {
                        market_id: "preview_market".to_string(), // In reality we map from OrderResult or intent
                        side: Side::Yes,
                        size: rust_decimal_macros::dec!(10.0),
                        entry_price: result.filled_at,
                        source_wallet: alloy_primitives::Address::default(),
                        opened_at: result.timestamp,
                        pnl: None,
                    };
                    self.position_tracker.add_position(position);

                    let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "SETTLE".to_string(),
                        message: format!("LIVE Settled TX: {:?}", tx_hash),
                    });
                }
            }
            OrderStatus::Rejected => {
                let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "ERR".to_string(),
                    message: format!("Order rejected: {}", result.order_id),
                });
                self.consecutive_losses.fetch_add(1, Ordering::SeqCst);
            }
            OrderStatus::Timeout => {
                let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "ERR".to_string(),
                    message: format!("Order timed out: {}", result.order_id),
                });
                self.consecutive_losses.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}
