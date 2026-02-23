use futures::future::join_all;
use serde_json::Value;
use std::time::Duration;
use tracing::{info, error, warn};
use tokio::sync::broadcast;
use crate::types::{TradeEvent, Side};
use alloy_primitives::Address;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// RtdsEngine has been re-purposed as a Gamma REST API poller since the 
/// Polymarket CLOB WebSocket requires knowing ALL `asset_ids` in advance, 
/// making global wallet tracking impossible via a single WS connection.
pub struct RtdsEngine {
    tx: broadcast::Sender<TradeEvent>,
    watched_wallets: Vec<Address>,
    seen_hashes: Arc<RwLock<std::collections::HashSet<String>>>,
    client: reqwest::Client,
}

impl RtdsEngine {
    pub fn new(tx: broadcast::Sender<TradeEvent>, wallets: Vec<Address>) -> Self {
        Self {
            tx,
            watched_wallets: wallets,
            seen_hashes: Arc::new(RwLock::new(std::collections::HashSet::new())),
            client: reqwest::Client::new(),
        }
    }

    pub async fn run(&self) {
        if self.watched_wallets.is_empty() {
            warn!("No wallets configured for tracking. Gamma API poller idle.");
            return;
        }

        info!("Starting Gamma API poller for {} wallets", self.watched_wallets.len());

        let mut backoff = Duration::from_secs(2);
        
        loop {
            let mut tasks = vec![];
            
            for wallet in &self.watched_wallets {
                let wallet_str = format!("{:?}", wallet);
                let client = self.client.clone();
                let seen_hashes = self.seen_hashes.clone();
                let tx = self.tx.clone();
                
                tasks.push(tokio::spawn(async move {
                    Self::poll_wallet(client, wallet_str, seen_hashes, tx).await;
                }));
            }
            
            // Wait for all wallet polls to finish
            join_all(tasks).await;
            
            // Cleanup cache every so often to prevent memory leak (rough size bound)
            {
                let mut cache = self.seen_hashes.write().await;
                if cache.len() > 10000 {
                    cache.clear();
                }
            }

            tokio::time::sleep(backoff).await;
        }
    }

    async fn poll_wallet(
        client: reqwest::Client, 
        wallet: String, 
        seen_hashes: Arc<RwLock<std::collections::HashSet<String>>>,
        tx: broadcast::Sender<TradeEvent>
    ) {
        // Polymarket Data API - public endpoint for user activity
        // Returns: transactionHash, asset, side (BUY/SELL), price, size, usdcSize, title, conditionId, timestamp
        let url = format!(
            "https://data-api.polymarket.com/activity?user={}&limit=30&offset=0",
            wallet
        );
        
        match client.get(&url).send().await {
            Ok(res) => {
                if let Ok(json) = res.json::<Value>().await {
                    if let Some(trades) = json.as_array() {
                        let mut cache = seen_hashes.write().await;
                        for trade in trades {
                            // Only process TRADE type events
                            let trade_type = trade.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if trade_type != "TRADE" {
                                continue;
                            }

                            // Use transactionHash as unique dedup key
                            let tx_hash = trade.get("transactionHash").and_then(|v| v.as_str()).unwrap_or("");
                            if tx_hash.is_empty() || cache.contains(tx_hash) {
                                continue;
                            }
                            cache.insert(tx_hash.to_string());
                            
                            // Extract trade details from Data API response
                            let asset_id = trade.get("asset").and_then(|v| v.as_str()).unwrap_or("");
                            let side_str = trade.get("side").and_then(|v| v.as_str()).unwrap_or("BUY");
                            let condition_id = trade.get("conditionId").and_then(|v| v.as_str()).unwrap_or("");
                            let title = trade.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown");

                            // Data API returns numbers directly, not strings
                            let price = trade.get("price")
                                .and_then(|v| v.as_f64())
                                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                .unwrap_or_default();
                            let size = trade.get("size")
                                .and_then(|v| v.as_f64())
                                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                .unwrap_or_default();
                            let usdc_size = trade.get("usdcSize")
                                .and_then(|v| v.as_f64())
                                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                .unwrap_or(size);
                            let timestamp = trade.get("timestamp")
                                .and_then(|v| v.as_u64())
                                .unwrap_or_else(|| chrono::Utc::now().timestamp() as u64);

                            if asset_id.is_empty() {
                                continue;
                            }
                            
                            let side = if side_str.eq_ignore_ascii_case("BUY") { Side::Yes } else { Side::No };

                            if let Ok(wallet_addr) = Address::from_str(&wallet) {
                                let event = TradeEvent {
                                    asset_id: asset_id.to_string(),
                                    wallet: wallet_addr,
                                    side,
                                    size: usdc_size,  // Use USDC size for copy sizing
                                    price,
                                    market_id: condition_id.to_string(),
                                    timestamp_ms: timestamp * 1000,
                                };
                                info!("[RTDS] New trade: {} {} ${} @ {} — {}", 
                                    side_str, title, usdc_size, price, &wallet[..10]);
                                let _ = tx.send(event);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch activity for {}: {}", wallet, e);
            }
        }
    }
}

