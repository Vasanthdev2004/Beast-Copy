use serde_json::Value;
use std::time::Duration;
use tracing::{info, warn, error};
use tokio::sync::broadcast;
use crate::types::{TradeEvent, Side};
use crate::types::LogEntry;
use crate::engines::wallet_tracker::WalletTracker;
use alloy_primitives::Address;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

/// WebSocket endpoint for Polymarket Real-Time Data Streaming.
/// Streams ALL platform trades in real-time including wallet addresses.
/// No authentication required.
const WS_URL: &str = "wss://ws-live-data.polymarket.com/ws/trades";

/// Maximum age of a trade (in seconds) before it is considered stale.
const STALENESS_CUTOFF_SECS: u64 = 30;

/// How long to wait before attempting reconnection after a WS drop.
const RECONNECT_DELAY_SECS: u64 = 3;

/// REST polling interval used as fallback when WS is disconnected.
const REST_POLL_INTERVAL_SECS: u64 = 2;

/// Real-Time Data Streaming engine.
///
/// Primary mode: WebSocket stream from `wss://ws-live-data.polymarket.com`
/// receiving every trade on the platform and filtering client-side by watched wallets.
///
/// Fallback mode: REST polling against `data-api.polymarket.com/activity`
/// (activated automatically if WebSocket connection fails).
pub struct RtdsEngine {
    tx: broadcast::Sender<TradeEvent>,
    log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    wallet_tracker: Arc<WalletTracker>,
    /// Hash → timestamp (Unix seconds) dedup cache with TTL eviction.
    seen_hashes: Arc<DashMap<String, u64>>,
    client: reqwest::Client,
}

impl RtdsEngine {
    pub fn new(
        tx: broadcast::Sender<TradeEvent>,
        wallet_tracker: Arc<WalletTracker>,
        log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PolyApex/1.0")
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            tx,
            log_tx,
            wallet_tracker,
            seen_hashes: Arc::new(DashMap::new()),
            client,
        }
    }

    // ─────────────────────────────────────────────────────────────
    //  Primary entry point: tries WS, falls back to REST on failure
    // ─────────────────────────────────────────────────────────────
    pub async fn run(&self) {
        info!("RTDS Engine starting — attempting WebSocket stream...");

        loop {
            // Attempt WebSocket connection
            match self.run_websocket().await {
                Ok(()) => {
                    warn!("WebSocket stream ended cleanly — reconnecting in {}s...", RECONNECT_DELAY_SECS);
                }
                Err(e) => {
                    error!("WebSocket connection failed: {} — falling back to REST polling", e);
                    let _ = self.log_tx.send(LogEntry {
                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                        kind: "WARN".to_string(),
                        message: format!("WS down: {} — REST fallback active", e),
                    });

                    // Run REST fallback for a window, then retry WS
                    self.run_rest_fallback(30).await;
                }
            }

            // Brief pause before reconnect attempt
            tokio::time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
        }
    }

    // ─────────────────────────────────────────────────────────────
    //  WebSocket mode: real-time stream with client-side filtering
    // ─────────────────────────────────────────────────────────────
    async fn run_websocket(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(WS_URL).await?;
        let (mut write, mut read) = ws_stream.split();

        info!("✅ WebSocket connected to {}", WS_URL);
        let _ = self.log_tx.send(LogEntry {
            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
            kind: "INFO".to_string(),
            message: "WebSocket RTDS stream connected — real-time mode active".to_string(),
        });

        // Subscribe to activity/trades stream (all trades, no auth)
        // The Polymarket RTDS protocol expects a subscription message.
        // Server-side filtering by slug is broken, so we use empty filter
        // and filter client-side by watched wallet addresses.
        let subscribe_msg = serde_json::json!({
            "type": "subscribe",
            "channel": "trades",
        });
        write.send(Message::Text(subscribe_msg.to_string().into())).await?;
        info!("Subscribed to RTDS trades channel");

        // Periodic eviction + heartbeat
        let mut evict_interval = tokio::time::interval(Duration::from_secs(300));
        evict_interval.tick().await; // consume first immediate tick

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.process_ws_message(&text).await;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = write.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) => {
                            warn!("WebSocket received close frame");
                            return Ok(());
                        }
                        Some(Err(e)) => {
                            return Err(Box::new(e));
                        }
                        None => {
                            warn!("WebSocket stream ended");
                            return Ok(());
                        }
                        _ => {} // Binary, Pong, Frame — ignore
                    }
                }
                _ = evict_interval.tick() => {
                    let now_ts = chrono::Utc::now().timestamp() as u64;
                    self.seen_hashes.retain(|_, ts| now_ts.saturating_sub(*ts) < 3600);
                }
            }
        }
    }

    /// Parse a single WebSocket message from the RTDS stream.
    /// The stream broadcasts ALL platform trades; we filter client-side
    /// to only process trades from our watched wallets.
    async fn process_ws_message(&self, text: &str) {
        // The RTDS can send arrays of trade objects or single objects
        let messages: Vec<Value> = if text.starts_with('[') {
            serde_json::from_str(text).unwrap_or_default()
        } else {
            match serde_json::from_str::<Value>(text) {
                Ok(v) => vec![v],
                Err(_) => return,
            }
        };

        let now_ts = chrono::Utc::now().timestamp() as u64;

        for trade in messages {
            // Extract the trader's wallet/proxy address
            let trader = trade.get("maker_address")
                .or_else(|| trade.get("taker_address"))
                .or_else(|| trade.get("owner"))
                .or_else(|| trade.get("trader"))
                .or_else(|| trade.get("user"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if trader.is_empty() {
                continue;
            }

            // Parse wallet address and check if it's one of our watched whales
            let wallet_addr = match Address::from_str(trader) {
                Ok(a) => a,
                Err(_) => continue,
            };

            if !self.wallet_tracker.is_watched(&wallet_addr) {
                continue;  // Not our whale — skip (this is the high-speed filter)
            }

            // ── Dedup by transaction hash ──
            let tx_hash = trade.get("transaction_hash")
                .or_else(|| trade.get("transactionHash"))
                .or_else(|| trade.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if tx_hash.is_empty() {
                continue;
            }

            if self.seen_hashes.contains_key(tx_hash) {
                continue;
            }
            self.seen_hashes.insert(tx_hash.to_string(), now_ts);

            // ── Extract trade fields ──
            let asset_id = trade.get("asset_id")
                .or_else(|| trade.get("asset"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let side_str = trade.get("side")
                .and_then(|v| v.as_str())
                .unwrap_or("BUY");

            let condition_id = trade.get("market")
                .or_else(|| trade.get("conditionId"))
                .or_else(|| trade.get("condition_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let outcome = trade.get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let price = trade.get("price")
                .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                .unwrap_or_default();

            let usdc_size = trade.get("size")
                .or_else(|| trade.get("usdcSize"))
                .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                .unwrap_or_default();

            let timestamp = trade.get("timestamp")
                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                .unwrap_or(now_ts);

            if asset_id.is_empty() {
                continue;
            }

            // ── Staleness check ──
            let age_secs = now_ts.saturating_sub(timestamp);
            if age_secs > STALENESS_CUTOFF_SECS {
                let _ = self.log_tx.send(LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "HIST".to_string(),
                    message: format!("WS {} {} ${:.2} @¢{} [STALE {}s]",
                        side_str, outcome, usdc_size.round_dp(2),
                        (price * Decimal::from(100)).round_dp(0), age_secs),
                });
                continue;
            }

            let is_sell = side_str.eq_ignore_ascii_case("SELL");
            let side = if outcome.eq_ignore_ascii_case("No") { Side::No } else { Side::Yes };

            let event = TradeEvent {
                asset_id: asset_id.to_string(),
                wallet: wallet_addr,
                side,
                size: usdc_size,
                price,
                market_id: condition_id.to_string(),
                timestamp_ms: timestamp * 1000,
                is_sell,
            };

            let _ = self.log_tx.send(LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "⚡WS".to_string(),
                message: format!("{} {} ${:.2} @¢{} — {}",
                    side_str, outcome, usdc_size.round_dp(2),
                    (price * Decimal::from(100)).round_dp(0),
                    &format!("{:?}", wallet_addr)[..10]),
            });

            let _ = self.tx.send(event);
        }
    }

    // ─────────────────────────────────────────────────────────────
    //  REST fallback: polls data-api when WS is down
    // ─────────────────────────────────────────────────────────────
    async fn run_rest_fallback(&self, cycles: usize) {
        info!("REST fallback active for {} cycles", cycles);
        let poll_interval = Duration::from_secs(REST_POLL_INTERVAL_SECS);

        for _ in 0..cycles {
            let watched_wallets: Vec<Address> = self.wallet_tracker.scores.iter()
                .map(|entry| *entry.key())
                .collect();

            if watched_wallets.is_empty() {
                tokio::time::sleep(poll_interval).await;
                continue;
            }

            let mut tasks = vec![];
            for wallet in &watched_wallets {
                let wallet_str = format!("{:?}", wallet).to_lowercase();
                let client = self.client.clone();
                let seen_hashes = self.seen_hashes.clone();
                let tx = self.tx.clone();
                let log_tx = self.log_tx.clone();

                tasks.push(tokio::spawn(async move {
                    Self::poll_wallet_rest(client, wallet_str, seen_hashes, tx, log_tx).await;
                }));
            }

            futures::future::join_all(tasks).await;

            // Evict stale hashes
            let now_ts = chrono::Utc::now().timestamp() as u64;
            self.seen_hashes.retain(|_, ts| now_ts.saturating_sub(*ts) < 3600);

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn poll_wallet_rest(
        client: reqwest::Client,
        wallet: String,
        seen_hashes: Arc<DashMap<String, u64>>,
        tx: broadcast::Sender<TradeEvent>,
        log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    ) {
        let url = format!(
            "https://data-api.polymarket.com/activity?user={}&limit=30&offset=0",
            wallet
        );

        let mut attempts = 0;
        let max_retries = 3;

        loop {
            match client.get(&url).send().await {
                Ok(res) => {
                    if let Ok(json) = res.json::<Value>().await {
                        if let Some(trades) = json.as_array() {
                            let now_ts = chrono::Utc::now().timestamp() as u64;
                            for trade in trades {
                                let trade_type = trade.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                if trade_type != "TRADE" { continue; }

                                let tx_hash = trade.get("transactionHash").and_then(|v| v.as_str()).unwrap_or("");
                                if tx_hash.is_empty() { continue; }

                                let timestamp = trade.get("timestamp")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(now_ts);

                                let age_secs = now_ts.saturating_sub(timestamp);
                                if age_secs > STALENESS_CUTOFF_SECS {
                                    seen_hashes.insert(tx_hash.to_string(), now_ts);
                                    continue;
                                }

                                if seen_hashes.contains_key(tx_hash) { continue; }
                                seen_hashes.insert(tx_hash.to_string(), now_ts);

                                let asset_id = trade.get("asset").and_then(|v| v.as_str()).unwrap_or("");
                                let side_str = trade.get("side").and_then(|v| v.as_str()).unwrap_or("BUY");
                                let condition_id = trade.get("conditionId").and_then(|v| v.as_str()).unwrap_or("");
                                let outcome = trade.get("outcome").and_then(|v| v.as_str()).unwrap_or("");

                                let price = trade.get("price")
                                    .and_then(|v| v.as_f64())
                                    .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                    .unwrap_or_default();
                                let usdc_size = trade.get("usdcSize")
                                    .and_then(|v| v.as_f64())
                                    .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                                    .unwrap_or_default();

                                if asset_id.is_empty() { continue; }

                                let is_sell = side_str.eq_ignore_ascii_case("SELL");
                                let side = if outcome.eq_ignore_ascii_case("No") { Side::No } else { Side::Yes };

                                if let Ok(wallet_addr) = Address::from_str(&wallet) {
                                    let event = TradeEvent {
                                        asset_id: asset_id.to_string(),
                                        wallet: wallet_addr,
                                        side,
                                        size: usdc_size,
                                        price,
                                        market_id: condition_id.to_string(),
                                        timestamp_ms: timestamp * 1000,
                                        is_sell,
                                    };

                                    let _ = log_tx.send(LogEntry {
                                        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                                        kind: "REST".to_string(),
                                        message: format!("{} {} ${:.2} @¢{} — {}",
                                            side_str, outcome, usdc_size.round_dp(2),
                                            (price * Decimal::from(100)).round_dp(0),
                                            &format!("{:?}", wallet_addr)[..10]),
                                    });

                                    let _ = tx.send(event);
                                }
                            }
                        }
                    }
                    break;
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_retries {
                        warn!("REST fallback failed for {} after {} retries: {}", wallet, max_retries, e);
                        break;
                    }
                    let backoff = Duration::from_millis(500 * (1 << (attempts - 1)));
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
}
