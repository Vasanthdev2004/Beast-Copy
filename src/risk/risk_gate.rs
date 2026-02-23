use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, error};
use rust_decimal::Decimal;

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::types::{OrderIntent, BotState};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;

pub struct RiskGate {
    intent_rx: mpsc::Receiver<OrderIntent>,
    clob_tx: mpsc::Sender<OrderIntent>,
    bot_state: Arc<RwLock<BotState>>,
    config: Arc<RwLock<AppConfig>>,
    position_tracker: Arc<PositionTracker>,
    consecutive_losses: Arc<AtomicUsize>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
}

impl RiskGate {
    pub fn new(
        intent_rx: mpsc::Receiver<OrderIntent>,
        clob_tx: mpsc::Sender<OrderIntent>,
        bot_state: Arc<RwLock<BotState>>,
        config: Arc<RwLock<AppConfig>>,
        position_tracker: Arc<PositionTracker>,
        consecutive_losses: Arc<AtomicUsize>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
    ) -> Self {
        Self {
            intent_rx,
            clob_tx,
            bot_state,
            config,
            position_tracker,
            consecutive_losses,
            log_tx,
        }
    }

    pub async fn run(mut self) {
        info!("RiskGate started");
        while let Some(intent) = self.intent_rx.recv().await {
            if self.pre_trade_checks(&intent).await {
                if let Err(e) = self.clob_tx.send(intent).await {
                    error!("RiskGate failed to send to CLOB executor: {}", e);
                }
            } else {
                warn!("RiskGate rejected order intent for market {}", intent.market_id);
            }
        }
    }

    async fn pre_trade_checks(&mut self, intent: &OrderIntent) -> bool {
        if *self.bot_state.read().await != BotState::Running {
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "SKIP".to_string(),
                message: "Bot paused, skipped intent".to_string(),
            });
            return false;
        }

        let config = self.config.read().await;

        let min_size = Decimal::from_f64_retain(config.copy.min_copy_size_usdc).unwrap_or(rust_decimal_macros::dec!(2.0));
        if intent.size < min_size {
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "SKIP".to_string(),
                message: format!("Size {} below min {}", intent.size, min_size),
            });
            return false;
        }

        let losses = self.consecutive_losses.load(Ordering::SeqCst);
        if losses >= config.risk.consecutive_loss_halt {
            *self.bot_state.write().await = BotState::Halted;
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "RISK".to_string(),
                message: format!("HALTED: {} consecutive losses", losses),
            });
            return false;
        }

        let open_positions = self.position_tracker.get_total_open_positions();
        if open_positions >= config.risk.max_open_positions {
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "RISK".to_string(),
                message: format!("Max open positions ({}) reached", open_positions),
            });
            return false;
        }
        
        // Extended checks for slippage and portfolio max open pos would happen here, querying PositionTracker/MemoryStore
        // Simple slippage check (assuming intent.price is already close to market odds)
        if intent.price < rust_decimal_macros::dec!(0.01) || intent.price > rust_decimal_macros::dec!(0.99) {
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "SKIP".to_string(),
                message: format!("Price {} outside safe bounds (0.01-0.99)", intent.price),
            });
            return false;
        }
        
        true
    }
}
