use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn, error};

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::types::{TradeEvent, OrderIntent, BotState, OrderType};
use crate::config::AppConfig;
use crate::engines::wallet_tracker::WalletTracker;
use crate::utils::math::calculate_kelly_size;
use tokio::sync::RwLock;

pub struct CopyEngine {
    trade_rx: broadcast::Receiver<TradeEvent>,
    intent_tx: mpsc::Sender<OrderIntent>,
    wallet_tracker: Arc<WalletTracker>,
    config: Arc<RwLock<AppConfig>>,
    bot_state: Arc<RwLock<BotState>>,
    recent_trades: dashmap::DashMap<String, u64>,
}

impl CopyEngine {
    pub fn new(
        trade_rx: broadcast::Receiver<TradeEvent>,
        intent_tx: mpsc::Sender<OrderIntent>,
        wallet_tracker: Arc<WalletTracker>,
        config: Arc<RwLock<AppConfig>>,
        bot_state: Arc<RwLock<BotState>>,
    ) -> Self {
        Self {
            trade_rx,
            intent_tx,
            wallet_tracker,
            config,
            bot_state,
            recent_trades: dashmap::DashMap::new(),
        }
    }

    pub async fn run(mut self) {
        info!("CopyEngine started");
        loop {
            match self.trade_rx.recv().await {
                Ok(event) => {
                    self.process_trade(event).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("CopyEngine lagged behind by {} messages", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    warn!("CopyEngine trade channel closed");
                    break;
                }
            }
        }
    }

    async fn process_trade(&self, event: TradeEvent) {
        if *self.bot_state.read().await != BotState::Running {
            return;
        }

        let config = self.config.read().await;

        let dedup_key = format!("{}-{}-{:?}", event.wallet, event.market_id, event.side);
        let now = chrono::Utc::now().timestamp_millis() as u64;
        
        if let Some(mut last_seen) = self.recent_trades.get_mut(&dedup_key) {
            if now - *last_seen < config.copy.dedup_window_secs * 1000 {
                return;
            }
            *last_seen = now;
        } else {
            self.recent_trades.insert(dedup_key.clone(), now);
        }

        let Some(score) = self.wallet_tracker.get_score(&event.wallet) else {
            return;
        };

        // Dummy portfolio balance for kelly sizing
        let portfolio_balance = dec!(1000.0);

        let mut size = match config.copy.sizing_mode.as_str() {
            "kelly" => calculate_kelly_size(score.win_rate, event.price, portfolio_balance),
            "percent" => {
                let ratio = Decimal::from_f64_retain(config.copy.copy_ratio).unwrap_or(dec!(0.5));
                event.size * ratio
            }
            "fixed" => Decimal::from_f64_retain(config.copy.fixed_usdc).unwrap_or(dec!(10.0)),
            _ => dec!(0.0),
        };

        let max_size = Decimal::from_f64_retain(config.copy.max_single_trade_usdc).unwrap_or(dec!(100.0));
        if size > max_size {
            size = max_size;
        }

        let min_size = Decimal::from_f64_retain(config.copy.min_copy_size_usdc).unwrap_or(dec!(2.0));
        if size < min_size {
            return; // Too small
        }

        let intent = OrderIntent {
            market_id: event.market_id,
            asset_id: event.asset_id,
            side: event.side,
            size,
            price: event.price,
            order_type: OrderType::FOK,
            source_wallet: event.wallet,
        };

        if let Err(e) = self.intent_tx.send(intent).await {
            error!("Failed to send OrderIntent to RiskGate: {}", e);
        }
    }
}
