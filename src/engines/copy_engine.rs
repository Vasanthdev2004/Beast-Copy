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
    portfolio_balance: Arc<RwLock<Decimal>>,
    recent_trades: Arc<dashmap::DashMap<String, u64>>,
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
            portfolio_balance: Arc::new(RwLock::new(dec!(0.0))),
            recent_trades: Arc::new(dashmap::DashMap::new()),
        }
    }

    pub async fn run(mut self) {
        info!("CopyEngine started");
        
        let recent_trades_clone = self.recent_trades.clone();
        let config_clone = self.config.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let dedup_secs = config_clone.read().await.copy.dedup_window_secs;
                let cutoff = chrono::Utc::now().timestamp_millis() as u64 - (dedup_secs * 1000);
                recent_trades_clone.retain(|_, ts| *ts > cutoff);
            }
        });

        let balance_clone = self.portfolio_balance.clone();
        let config_rpc = self.config.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            // USDC.e token address on Polygon, commonly used by Polymarket
            let usdc_address = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
            loop {
                let rpc_url = config_rpc.read().await.rpc.polygon_rpc.clone();
                // Very basic derive public address from private key (or just use public key config)
                // Since this runs locally, we can assume the user configures their public address or we derive it via alloy
                // Wait, alloy can derive address from private key. Since we need it for balance, let's use a dummy for now 
                // and later refactor clob_executor to provide it. For now, we fetch safely if WALLET_PUBLIC_ADDRESS is set.
                let wallet_address = std::env::var("WALLET_PUBLIC_ADDRESS").unwrap_or_else(|_| "0x0000000000000000000000000000000000000000".to_string());
                
                if wallet_address != "0x0000000000000000000000000000000000000000" {
                    let mut data = "0x70a08231000000000000000000000000".to_string();
                    data.push_str(wallet_address.trim_start_matches("0x"));

                    let payload = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "eth_call",
                        "params": [{
                            "to": usdc_address,
                            "data": data
                        }, "latest"],
                        "id": 1
                    });

                    if let Ok(res) = client.post(&rpc_url).json(&payload).send().await {
                        if let Ok(json) = res.json::<serde_json::Value>().await {
                            if let Some(result) = json.get("result").and_then(|r| r.as_str()) {
                                let clean_hex = result.trim_start_matches("0x");
                                if let Ok(val) = u64::from_str_radix(clean_hex, 16) {
                                    let balance = Decimal::from_f64_retain((val as f64) / 1_000_000.0).unwrap_or(dec!(0.0));
                                    *balance_clone.write().await = balance;
                                }
                            }
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });

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

        // Read portfolio balance correctly from fetched state
        let portfolio_balance = *self.portfolio_balance.read().await;

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
