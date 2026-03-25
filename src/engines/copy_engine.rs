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
use std::str::FromStr;

pub struct CopyEngine {
    trade_rx: broadcast::Receiver<TradeEvent>,
    intent_tx: mpsc::Sender<OrderIntent>,
    wallet_tracker: Arc<WalletTracker>,
    config: Arc<RwLock<AppConfig>>,
    bot_state: Arc<RwLock<BotState>>,
    portfolio_balance: Arc<RwLock<Decimal>>,
    recent_trades: Arc<dashmap::DashMap<String, u64>>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
}

impl CopyEngine {
    pub fn new(
        trade_rx: broadcast::Receiver<TradeEvent>,
        intent_tx: mpsc::Sender<OrderIntent>,
        wallet_tracker: Arc<WalletTracker>,
        config: Arc<RwLock<AppConfig>>,
        bot_state: Arc<RwLock<BotState>>,
        portfolio_balance: Arc<RwLock<Decimal>>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
    ) -> Self {
        Self {
            trade_rx,
            intent_tx,
            wallet_tracker,
            config,
            bot_state,
            portfolio_balance,
            recent_trades: Arc::new(dashmap::DashMap::new()),
            log_tx,
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

        // Only poll on-chain USDC balance in LIVE mode (not paper trading)
        let is_preview = self.config.read().await.copy.preview_mode;
        if !is_preview {
            let balance_clone = self.portfolio_balance.clone();
            let config_rpc = self.config.clone();
            tokio::spawn(async move {
                let client = reqwest::Client::new();
                let usdc_address = "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"; // Native USDC on Polygon
                loop {
                    let rpc_url = config_rpc.read().await.rpc.polygon_rpc.clone();
                    let mut wallet_address = std::env::var("WALLET_PUBLIC_ADDRESS").unwrap_or_default();
                    if wallet_address.is_empty() {
                        if let Ok(pk) = std::env::var("WALLET_PRIVATE_KEY") {
                            if let Ok(signer) = alloy_signer_local::PrivateKeySigner::from_str(&pk) {
                                wallet_address = format!("{:?}", signer.address());
                            }
                        }
                    }
                    
                    if !wallet_address.is_empty() && wallet_address != "0x0000000000000000000000000000000000000000" {
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
        }

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
            "percent" | "ratio" => {
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
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "SKIP".to_string(),
                message: format!("Calculated size ${:.2} < Min ${:.2}", size, min_size),
            });
            return;
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

        let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
            kind: "COPY".to_string(),
            message: format!("Wallet 0x{}…{} size: ${}", 
                &format!("{:?}", event.wallet)[2..6],
                &format!("{:?}", event.wallet)[38..42],
                size),
        });

        if let Err(e) = self.intent_tx.send(intent).await {
            error!("Failed to send OrderIntent to RiskGate: {}", e);
        }
    }
}
