use futures::future::join_all;
use serde_json::Value;
use std::time::Duration;
use tracing::{info, warn};
use tokio::sync::broadcast;
use crate::types::{TradeEvent, Side};
use crate::tui::dashboard::LogEntry;
use alloy_primitives::Address;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Polls the Polymarket Data API for wallet trade activity.
/// On first run it seeds recent trades into the TUI feed (without copy-trading them).
/// On subsequent polls it broadcasts genuinely new trades to the CopyEngine pipeline.
pub struct RtdsEngine {
    tx: broadcast::Sender<TradeEvent>,
    log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    watched_wallets: Vec<Address>,
    seen_hashes: Arc<RwLock<std::collections::HashSet<String>>>,
    client: reqwest::Client,
}

impl RtdsEngine {
    pub fn new(
        tx: broadcast::Sender<TradeEvent>,
        wallets: Vec<Address>,
        log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PolyApex/1.0")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            tx,
            log_tx,
            watched_wallets: wallets,
            seen_hashes: Arc::new(RwLock::new(std::collections::HashSet::new())),
            client,
        }
    }

    pub async fn run(&self) {
        if self.watched_wallets.is_empty() {
            warn!("No wallets configured for tracking. Data API poller idle.");
            return;
        }

        info!("Starting Data API poller for {} wallets", self.watched_wallets.len());

        let poll_interval = Duration::from_secs(2);
        let mut first_run = true;
        
        loop {
            let mut tasks = vec![];
            
            for wallet in &self.watched_wallets {
                let wallet_str = format!("{:?}", wallet).to_lowercase();
                let client = self.client.clone();
                let seen_hashes = self.seen_hashes.clone();
                let tx = self.tx.clone();
                let log_tx = self.log_tx.clone();
                let seed_only = first_run;
                
                tasks.push(tokio::spawn(async move {
                    Self::poll_wallet(client, wallet_str, seen_hashes, tx, log_tx, seed_only).await;
                }));
            }
            
            join_all(tasks).await;
            
            // Evict oldest entries to prevent unbounded memory growth
            {
                let mut cache = self.seen_hashes.write().await;
                if cache.len() > 10000 {
                    let skip = cache.len() - 5000;
                    let to_keep: Vec<String> = cache.iter().skip(skip).cloned().collect();
                    cache.clear();
                    for h in to_keep {
                        cache.insert(h);
                    }
                }
            }

            if first_run {
                first_run = false;
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn poll_wallet(
        client: reqwest::Client, 
        wallet: String, 
        seen_hashes: Arc<RwLock<std::collections::HashSet<String>>>,
        tx: broadcast::Sender<TradeEvent>,
        log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
        seed_only: bool,
    ) {
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

                            // Dedup by transactionHash
                            let tx_hash = trade.get("transactionHash").and_then(|v| v.as_str()).unwrap_or("");
                            if tx_hash.is_empty() || cache.contains(tx_hash) {
                                continue;
                            }
                            cache.insert(tx_hash.to_string());
                            
                            // Extract fields from Data API response
                            let asset_id = trade.get("asset").and_then(|v| v.as_str()).unwrap_or("");
                            let side_str = trade.get("side").and_then(|v| v.as_str()).unwrap_or("BUY");
                            let condition_id = trade.get("conditionId").and_then(|v| v.as_str()).unwrap_or("");
                            let title = trade.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown");
                            let outcome = trade.get("outcome").and_then(|v| v.as_str()).unwrap_or("");

                            let price = trade.get("price")
                                .and_then(|v| v.as_f64())
                                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                .unwrap_or_default();
                            let usdc_size = trade.get("usdcSize")
                                .and_then(|v| v.as_f64())
                                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                .unwrap_or_default();
                            let timestamp = trade.get("timestamp")
                                .and_then(|v| v.as_u64())
                                .unwrap_or_else(|| chrono::Utc::now().timestamp() as u64);

                            if asset_id.is_empty() {
                                continue;
                            }
                            
                            // Map side correctly: BUY of "No" outcome = Side::No
                            let side = if outcome.eq_ignore_ascii_case("No") {
                                if side_str.eq_ignore_ascii_case("BUY") { Side::No } else { Side::Yes }
                            } else {
                                if side_str.eq_ignore_ascii_case("BUY") { Side::Yes } else { Side::No }
                            };

                            // On first run: show in TUI feed but DON'T trigger copy engine
                            if seed_only {
                                let ts_str = chrono::DateTime::from_timestamp(timestamp as i64, 0)
                                    .map(|dt| dt.format("%H:%M:%S").to_string())
                                    .unwrap_or_else(|| "??:??:??".to_string());
                                    
                                let _ = log_tx.send(LogEntry {
                                    time: ts_str,
                                    kind: "HIST".to_string(),
                                    message: format!("{} {} ${:.2} @¢{} — {}", 
                                        side_str, outcome, usdc_size.round_dp(2), 
                                        (price * Decimal::from(100)).round_dp(0),
                                        if title.len() > 35 { &title[..35] } else { title }),
                                });
                                continue;
                            }

                            // Live run: broadcast to CopyEngine pipeline
                            if let Ok(wallet_addr) = Address::from_str(&wallet) {
                                let event = TradeEvent {
                                    asset_id: asset_id.to_string(),
                                    wallet: wallet_addr,
                                    side,
                                    size: usdc_size,
                                    price,
                                    market_id: condition_id.to_string(),
                                    timestamp_ms: timestamp * 1000,
                                };
                                
                                let _ = log_tx.send(LogEntry {
                                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                                    kind: "COPY".to_string(),
                                    message: format!("{} {} ${:.2} @¢{} — {}", 
                                        side_str, outcome, usdc_size.round_dp(2), 
                                        (price * Decimal::from(100)).round_dp(0),
                                        if title.len() > 35 { &title[..35] } else { title }),
                                });
                                
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
