use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};
use rust_decimal::Decimal;

use crate::types::{OrderResult, OrderStatus, Position};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct SettlementMonitor {
    result_rx: mpsc::Receiver<OrderResult>,
    position_tracker: Arc<PositionTracker>,
    config: Arc<RwLock<AppConfig>>,
    consecutive_losses: Arc<AtomicUsize>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::types::LogEntry>,
    usdc_balance: Arc<RwLock<Decimal>>,
    daily_loss: Arc<RwLock<Decimal>>,
    /// Receives realized loss notifications from ClobExecutor
    loss_rx: tokio::sync::mpsc::UnboundedReceiver<Decimal>,
}

impl SettlementMonitor {
    pub fn new(
        result_rx: mpsc::Receiver<OrderResult>,
        position_tracker: Arc<PositionTracker>,
        config: Arc<RwLock<AppConfig>>,
        consecutive_losses: Arc<AtomicUsize>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::types::LogEntry>,
        usdc_balance: Arc<RwLock<Decimal>>,
        daily_loss: Arc<RwLock<Decimal>>,
        loss_rx: tokio::sync::mpsc::UnboundedReceiver<Decimal>,
    ) -> Self {
        Self {
            result_rx,
            position_tracker,
            config,
            consecutive_losses,
            log_tx,
            usdc_balance,
            daily_loss,
            loss_rx,
        }
    }

    pub async fn run(mut self) {
        info!("SettlementMonitor started");

        loop {
            tokio::select! {
                Some(result) = self.result_rx.recv() => {
                    self.process_result(result).await;
                }
                Some(loss_amount) = self.loss_rx.recv() => {
                    self.record_realized_loss(loss_amount).await;
                }
                else => break,
            }
        }
    }

    async fn process_result(&self, result: OrderResult) {
        let config = self.config.read().await;

        match result.status {
            OrderStatus::Filled => {
                // Reset consecutive losses on successful fill
                self.consecutive_losses.store(0, Ordering::SeqCst);

                // Insert confirmed position from actual order data
                let position = Position {
                    market_id: result.market_id.clone(),
                    side: result.side,
                    size: result.size,
                    entry_price: result.filled_at,
                    source_wallet: result.source_wallet,
                    opened_at: result.timestamp,
                    pnl: None,
                };
                self.position_tracker.add_position(position);

                let mode_str = if config.copy.preview_mode { "PAPER" } else { "LIVE" };
                if let Some(tx_hash) = result.tx_hash {
                    let _ = self.log_tx.send(crate::types::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "SETTLE".to_string(),
                        message: format!("{} Settled TX: {:?}", mode_str, tx_hash),
                    });
                } else {
                    let _ = self.log_tx.send(crate::types::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "SETTLE".to_string(),
                        message: format!("{} Settled: {} ${:.2}", mode_str, result.order_id, result.size.round_dp(2)),
                    });
                }
            }
            OrderStatus::Rejected => {
                // Rejection = order didn't go through, refund balance in paper mode
                if config.copy.preview_mode {
                    let mut bal = self.usdc_balance.write().await;
                    *bal += result.size;
                }
                
                let _ = self.log_tx.send(crate::types::LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "ERR".to_string(),
                    message: format!("Order rejected: {} (balance refunded)", result.order_id),
                });
                // NOTE: Rejections are NOT financial losses — don't increment consecutive_losses
            }
            OrderStatus::Timeout => {
                // Timeout = order didn't confirm, refund balance in paper mode
                if config.copy.preview_mode {
                    let mut bal = self.usdc_balance.write().await;
                    *bal += result.size;
                }
                
                let _ = self.log_tx.send(crate::types::LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "ERR".to_string(),
                    message: format!("Order timed out: {} (balance refunded)", result.order_id),
                });
                // NOTE: Timeouts are NOT financial losses — don't increment consecutive_losses
            }
        }
    }

    /// Record a realized trading loss (called when a position is closed at a loss)
    pub async fn record_realized_loss(&self, loss_amount: Decimal) {
        *self.daily_loss.write().await += loss_amount;
        self.consecutive_losses.fetch_add(1, Ordering::SeqCst);
        warn!("Realized loss: ${:.2} | Daily total: ${:.2}", loss_amount, *self.daily_loss.read().await);
    }
}
