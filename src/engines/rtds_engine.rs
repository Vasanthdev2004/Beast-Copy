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
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// WebSocket endpoint for Polymarket Real-Time Data Streaming.
/// Streams ALL platform trades in real-time including wallet addresses.
/// No authentication required — but Origin header must be set.
const WS_URL: &str = "wss://ws-live-data.polymarket.com";

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
    //  Primary entry point: runs REST as backbone + WS as accelerator
    //  Both share the same `seen_hashes` dedup cache, so whichever
    //  detects a trade first wins — the other silently skips it.
    // ─────────────────────────────────────────────────────────────
    pub async fn run(&self) {
        info!("RTDS Engine starting — dual mode (REST backbone + WS accelerator)");

        // Spawn WebSocket stream as a parallel background task
        let ws_tx = self.tx.clone();
        let ws_log_tx = self.log_tx.clone();
        let ws_wt = self.wallet_tracker.clone();
        let ws_seen = self.seen_hashes.clone();

        tokio::spawn(async move {
            // Build a temporary self-like struct for the WS task
            let ws_engine = WsAccelerator {
                tx: ws_tx,
                log_tx: ws_log_tx,
                wallet_tracker: ws_wt,
                seen_hashes: ws_seen,
            };
            loop {
                match ws_engine.run_websocket().await {
                    Ok(()) => {
                        warn!("WebSocket stream ended — reconnecting in 5s...");
                    }
                    Err(e) => {
                        warn!("WebSocket error: {} — retrying in 5s", e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        // Main loop: REST polling backbone (proven, always works)
        let poll_interval = Duration::from_secs(REST_POLL_INTERVAL_SECS);
        let mut first_run = true;

        loop {
            let watched_wallets: Vec<Address> = self.wallet_tracker.scores.iter()
                .map(|entry| *entry.key())
                .collect();

            if watched_wallets.is_empty() {
                if first_run {
                    warn!("No wallets configured for tracking. Waiting...");
                    first_run = false;
                }
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
                let seed_only = first_run;

                tasks.push(tokio::spawn(async move {
                    Self::poll_wallet_rest(client, wallet_str, seen_hashes, tx, log_tx, seed_only).await;
                }));
            }

            futures::future::join_all(tasks).await;

            // Evict stale dedup entries
            let now_ts = chrono::Utc::now().timestamp() as u64;
            self.seen_hashes.retain(|_, ts| now_ts.saturating_sub(*ts) < 3600);

            if first_run {
                first_run = false;
                let _ = self.log_tx.send(LogEntry {
                    time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                    kind: "INFO".to_string(),
                    message: "RTDS engine running — REST backbone + WS accelerator".to_string(),
                });
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    async fn poll_wallet_rest(
        client: reqwest::Client,
        wallet: String,
        seen_hashes: Arc<DashMap<String, u64>>,
        tx: broadcast::Sender<TradeEvent>,
        log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
        seed_only: bool,
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
                                        kind: if seed_only { "HIST".to_string() } else { "COPY".to_string() },
                                        message: format!("{} {} ${:.2} @¢{} — {}",
                                            side_str, outcome, usdc_size.round_dp(2),
                                            (price * Decimal::from(100)).round_dp(0),
                                            &format!("{:?}", wallet_addr)[..10]),
                                    });

                                    if !seed_only {
                                        let _ = tx.send(event);
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_retries {
                        warn!("REST poll failed for {} after {} retries: {}", wallet, max_retries, e);
                        break;
                    }
                    let backoff = Duration::from_millis(500 * (1 << (attempts - 1)));
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
}

/// Lightweight struct that holds only the shared state needed
/// to run the WebSocket accelerator in its own tokio task.
struct WsAccelerator {
    tx: broadcast::Sender<TradeEvent>,
    log_tx: tokio::sync::mpsc::UnboundedSender<LogEntry>,
    wallet_tracker: Arc<WalletTracker>,
    seen_hashes: Arc<DashMap<String, u64>>,
}

impl WsAccelerator {
    async fn run_websocket(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut request = WS_URL.into_client_request()?;
        let headers = request.headers_mut();
        headers.insert("Origin", "https://polymarket.com".parse()?);
        headers.insert("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) PolyApex/1.0".parse()?);

        let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
        let (mut write, mut read) = ws_stream.split();

        info!("✅ WebSocket connected to {}", WS_URL);
        let _ = self.log_tx.send(LogEntry {
            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
            kind: "INFO".to_string(),
            message: "WS accelerator connected — real-time boost active".to_string(),
        });

        let subscribe_msg = serde_json::json!({
            "action": "subscribe",
            "subscriptions": [{
                "topic": "activity",
                "type": "trades",
                "filters": ""
            }]
        });
        write.send(Message::Text(subscribe_msg.to_string().into())).await?;

        let mut ping_interval = tokio::time::interval(Duration::from_secs(5));
        ping_interval.tick().await;
        let mut evict_interval = tokio::time::interval(Duration::from_secs(300));
        evict_interval.tick().await;

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
                        Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => return Ok(()),
                        _ => {}
                    }
                }
                _ = ping_interval.tick() => {
                    if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                        return Err(Box::new(e));
                    }
                }
                _ = evict_interval.tick() => {
                    let now_ts = chrono::Utc::now().timestamp() as u64;
                    self.seen_hashes.retain(|_, ts| now_ts.saturating_sub(*ts) < 3600);
                }
            }
        }
    }

    async fn process_ws_message(&self, text: &str) {
        if text.trim().is_empty() || text.trim() == "{}" || text.trim() == "[]" {
            return;
        }

        // The RTDS wraps each message in {"connection_id":"...","payload":{...}}
        let raw: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        // Unwrap the payload envelope
        let trade = if let Some(payload) = raw.get("payload") {
            payload
        } else {
            &raw
        };

        let now_ts = chrono::Utc::now().timestamp() as u64;

        // ── Wallet: RTDS uses "proxyWallet" ──
        let trader = trade.get("proxyWallet")
            .or_else(|| trade.get("proxy_wallet"))
            .or_else(|| trade.get("maker_address"))
            .or_else(|| trade.get("user"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if trader.is_empty() { return; }

        let wallet_addr = match Address::from_str(trader) {
            Ok(a) => a,
            Err(_) => return,
        };

        if !self.wallet_tracker.is_watched(&wallet_addr) { return; }

        // ── Dedup via connection_id ──
        let dedup_key = raw.get("connection_id")
            .or_else(|| trade.get("transactionHash"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let dedup = if dedup_key.is_empty() {
            format!("ws_{}_{}", trade.get("asset").and_then(|v| v.as_str()).unwrap_or(""), now_ts)
        } else {
            dedup_key.to_string()
        };

        if self.seen_hashes.contains_key(&dedup) { return; }
        self.seen_hashes.insert(dedup, now_ts);

        // ── Extract fields matching actual RTDS schema ──
        let asset_id = trade.get("asset")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let condition_id = trade.get("conditionId")
            .or_else(|| trade.get("condition_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let outcome = trade.get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // RTDS sends price as string "0.46"
        let price = trade.get("price")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
            .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
            .unwrap_or_default();

        let usdc_size = trade.get("size")
            .or_else(|| trade.get("usdcSize"))
            .and_then(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64()))
            .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
            .unwrap_or_default();

        if asset_id.is_empty() || condition_id.is_empty() { return; }

        let side_str = trade.get("side")
            .and_then(|v| v.as_str())
            .unwrap_or("BUY");
        let is_sell = side_str.eq_ignore_ascii_case("SELL");

        let side = if outcome.eq_ignore_ascii_case("No") || outcome.eq_ignore_ascii_case("Down") {
            Side::No
        } else {
            Side::Yes
        };

        let event = TradeEvent {
            asset_id: asset_id.to_string(),
            wallet: wallet_addr,
            side,
            size: usdc_size,
            price,
            market_id: condition_id.to_string(),
            timestamp_ms: now_ts * 1000,
            is_sell,
        };

        let name = trade.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let label = if !name.is_empty() && name.len() > 30 { &name[..30] } else if name.is_empty() { outcome } else { name };

        let _ = self.log_tx.send(LogEntry {
            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
            kind: "⚡WS".to_string(),
            message: format!("{} {} @¢{} — {}",
                side_str, outcome,
                (price * Decimal::from(100)).round_dp(0),
                label),
        });

        let _ = self.tx.send(event);
    }
}
