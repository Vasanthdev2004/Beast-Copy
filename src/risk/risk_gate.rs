use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn, error};
use rust_decimal::Decimal;

use crate::types::{OrderIntent, BotState};
use crate::config::AppConfig;

pub struct RiskGate {
    intent_rx: mpsc::Receiver<OrderIntent>,
    clob_tx: mpsc::Sender<OrderIntent>,
    bot_state: Arc<RwLock<BotState>>,
    config: Arc<RwLock<AppConfig>>,
    consecutive_losses: usize,
}

impl RiskGate {
    pub fn new(
        intent_rx: mpsc::Receiver<OrderIntent>,
        clob_tx: mpsc::Sender<OrderIntent>,
        bot_state: Arc<RwLock<BotState>>,
        config: Arc<RwLock<AppConfig>>,
    ) -> Self {
        Self {
            intent_rx,
            clob_tx,
            bot_state,
            config,
            consecutive_losses: 0,
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
            warn!("Bot is not running. Rejecting intent.");
            return false;
        }

        let config = self.config.read().await;

        let min_size = Decimal::from_f64_retain(config.copy.min_copy_size_usdc).unwrap_or(rust_decimal_macros::dec!(2.0));
        if intent.size < min_size {
            warn!("Order size {} below minimum {}", intent.size, min_size);
            return false;
        }

        if self.consecutive_losses >= config.risk.consecutive_loss_halt {
            *self.bot_state.write().await = BotState::Halted;
            warn!("CIRCUIT BREAKER: halted due to consecutive losses");
            return false;
        }
        
        // Extended checks for slippage and portfolio max open pos would happen here, querying PositionTracker/MemoryStore
        
        true
    }

    pub fn register_loss(&mut self) {
        self.consecutive_losses += 1;
    }
    
    pub fn register_win(&mut self) {
        self.consecutive_losses = 0;
    }
}
