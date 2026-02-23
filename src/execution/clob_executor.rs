use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, error};
use rust_decimal::Decimal;

use crate::types::{OrderIntent, OrderResult, OrderStatus, Side, Position};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;
use polymarket_rs::client::TradingClient;
use polymarket_rs::types::{OrderArgs, OrderType, ApiCreds, CreateOrderOptions};
use polymarket_rs::orders::OrderBuilder;
use alloy_signer_local::PrivateKeySigner;
use std::str::FromStr;

pub struct ClobExecutor {
    intent_rx: mpsc::Receiver<OrderIntent>,
    result_tx: mpsc::Sender<OrderResult>,
    config: Arc<RwLock<AppConfig>>,
    trading_client: Option<Arc<TradingClient>>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
    position_tracker: Arc<PositionTracker>,
    usdc_balance: Arc<RwLock<Decimal>>,
}

impl ClobExecutor {
    pub async fn new(
        intent_rx: mpsc::Receiver<OrderIntent>,
        result_tx: mpsc::Sender<OrderResult>,
        config: Arc<RwLock<AppConfig>>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::tui::dashboard::LogEntry>,
        position_tracker: Arc<PositionTracker>,
        usdc_balance: Arc<RwLock<Decimal>>,
    ) -> Self {
        let pkey_str = std::env::var("WALLET_PRIVATE_KEY").unwrap_or_default();
        let mut trading_client = None;
        
        if let Ok(_signer) = PrivateKeySigner::from_str(&pkey_str) {
            let poly_key = std::env::var("POLYMARKET_API_KEY").unwrap_or_default();
            let poly_secret = std::env::var("POLYMARKET_API_SECRET").unwrap_or_default();
            let poly_passphrase = std::env::var("POLYMARKET_API_PASSPHRASE").unwrap_or_default();
            
            if !poly_key.is_empty() {
                let api_creds = ApiCreds {
                    api_key: poly_key,
                    secret: poly_secret,
                    passphrase: poly_passphrase,
                };
                
                let eth_signer_builder = PrivateKeySigner::from_str(&pkey_str).unwrap();
                let eth_signer_client = PrivateKeySigner::from_str(&pkey_str).unwrap();
                let order_builder = OrderBuilder::new(eth_signer_builder, None, None);
                
                let client = TradingClient::new(
                    "https://clob.polymarket.com",
                    eth_signer_client,
                    137,
                    api_creds,
                    order_builder,
                );
                trading_client = Some(Arc::new(client));
            }
        }

        Self {
            intent_rx,
            result_tx,
            config,
            trading_client,
            log_tx,
            position_tracker,
            usdc_balance,
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
            // ── Paper Trading: deduct from balance, track position ──
            let cost = intent.size.round_dp(2);
            let price = intent.price.round_dp(2);
            
            {
                let mut bal = self.usdc_balance.write().await;
                if *bal < cost {
                    let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "SKIP".to_string(),
                        message: format!("Insufficient balance ${:.2} for ${:.2}", *bal, cost),
                    });
                    return;
                }
                *bal -= cost;
            }

            // Track as a paper position
            let position = Position {
                market_id: intent.market_id.clone(),
                side: intent.side,
                size: cost,
                entry_price: price,
                source_wallet: intent.source_wallet,
                opened_at: chrono::Utc::now().timestamp_millis() as u64,
                pnl: None,
            };
            self.position_tracker.positions.insert(
                format!("paper-{}", chrono::Utc::now().timestamp_millis()),
                position,
            );

            let side_str = match intent.side { Side::Yes => "YES", Side::No => "NO" };
            let bal = *self.usdc_balance.read().await;
            let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "FILL".to_string(),
                message: format!("{} ${:.2} @¢{} | bal ${:.2}", 
                    side_str, cost, (price * Decimal::from(100)).round_dp(0), bal),
            });
            
            let result = OrderResult {
                order_id: format!("paper-{}", chrono::Utc::now().timestamp_millis()),
                status: OrderStatus::Filled,
                tx_hash: None,
                filled_at: price,
                timestamp: chrono::Utc::now().timestamp_millis() as u64,
            };
            
            if let Err(e) = self.result_tx.send(result).await {
                error!("Failed to route preview result: {}", e);
            }
            return;
        }

        // ── Live Trading ──
        info!("Sending order to Polymarket CLOB: {:?}", intent);
        
        if let Some(client) = &self.trading_client {
            let side = match intent.side {
                Side::Yes => polymarket_rs::types::Side::Buy,
                Side::No => polymarket_rs::types::Side::Sell,
            };

            let ord = OrderArgs {
                token_id: intent.asset_id.clone(),
                price: intent.price,
                size: intent.size,
                side,
            };

            let options = CreateOrderOptions {
                tick_size: Some(rust_decimal_macros::dec!(0.01)),
                neg_risk: Some(false),
            };

            match client.create_and_post_order(&ord, None, None, options, OrderType::Fok).await {
                Ok(resp) => {
                    let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "FILL".to_string(),
                        message: format!("LIVE ${:.2} @¢{} ID:{}", 
                            intent.size.round_dp(2), 
                            (intent.price * Decimal::from(100)).round_dp(0),
                            resp.order_id),
                    });
                    
                    let result = OrderResult {
                        order_id: resp.order_id.as_str().to_string(),
                        status: OrderStatus::Filled,
                        tx_hash: None,
                        filled_at: intent.price,
                        timestamp: chrono::Utc::now().timestamp_millis() as u64,
                    };
                    let _ = self.result_tx.send(result).await;
                }
                Err(e) => {
                    let _ = self.log_tx.send(crate::tui::dashboard::LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "ERR".to_string(),
                        message: format!("Rejected: {:?}", e),
                    });
                    
                    let result = OrderResult {
                        order_id: "failed".to_string(),
                        status: OrderStatus::Rejected,
                        tx_hash: None,
                        filled_at: intent.price,
                        timestamp: chrono::Utc::now().timestamp_millis() as u64,
                    };
                    let _ = self.result_tx.send(result).await;
                }
            }
        } else {
            error!("CLOB Client not initialized. Check API keys in .env");
        }
    }
}
