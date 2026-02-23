use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures::{StreamExt, SinkExt};
use serde_json::Value;
use std::time::Duration;
use tracing::{info, error, warn};
use tokio::sync::broadcast;
use crate::types::{TradeEvent, Side};
use alloy_primitives::Address;
use rust_decimal::Decimal;
use std::str::FromStr;

pub struct RtdsEngine {
    url: String,
    tx: broadcast::Sender<TradeEvent>,
    watched_wallets: Vec<Address>,
}

impl RtdsEngine {
    pub fn new(tx: broadcast::Sender<TradeEvent>, wallets: Vec<Address>) -> Self {
        Self {
            url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            tx,
            watched_wallets: wallets,
        }
    }

    pub async fn run(&self) {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            info!("Connecting to RTDS at {}", self.url);
            match connect_async(&self.url).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to RTDS");
                    backoff = Duration::from_secs(1); // Reset backoff

                    let (mut write, mut read) = ws_stream.split();

                    let sub_msg = serde_json::json!({
                        "assets_ids": [],
                        "type": "market",
                        "action": "subscribe"
                    });
                    
                    if let Err(e) = write.send(Message::Text(sub_msg.to_string())).await {
                        error!("Failed to send subscribe message: {}", e);
                        continue;
                    }

                    // Ping task
                    let (ping_tx, mut ping_rx) = tokio::sync::mpsc::channel::<()>(1);
                    let ping_task = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            if ping_tx.send(()).await.is_err() {
                                break;
                            }
                        }
                    });

                    loop {
                        tokio::select! {
                            _ = ping_rx.recv() => {
                                if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                                    error!("Ping failed: {}", e);
                                    break;
                                }
                            }
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Text(text))) => {
                                        self.handle_message(&text);
                                    }
                                    Some(Ok(Message::Close(_))) | None => {
                                        warn!("RTDS WebSocket closed");
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        error!("RTDS WebSocket error: {}", e);
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    ping_task.abort();
                }
                Err(e) => {
                    error!("Failed to connect to RTDS: {}. Retrying in {:?}", e, backoff);
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                }
            }
        }
    }

    fn handle_message(&self, text: &str) {
        let Ok(val): Result<Value, _> = serde_json::from_str(text) else { return };
        
        if let Some(events) = val.as_array() {
            for event in events {
                if let (Some(wallet_str), Some(asset_id), Some(side_str), Some(size_str), Some(price_str)) = (
                    event.get("maker").and_then(|v| v.as_str()),
                    event.get("asset_id").and_then(|v| v.as_str()),
                    event.get("side").and_then(|v| v.as_str()),
                    event.get("size").and_then(|v| v.as_str()),
                    event.get("price").and_then(|v| v.as_str())
                ) {
                    if let Ok(wallet) = Address::from_str(wallet_str) {
                        if self.watched_wallets.contains(&wallet) {
                            let side = if side_str.eq_ignore_ascii_case("YES") {
                                Side::Yes
                            } else {
                                Side::No
                            };

                            let trade = TradeEvent {
                                asset_id: asset_id.to_string(),
                                wallet,
                                side,
                                size: Decimal::from_str(size_str).unwrap_or_default(),
                                price: Decimal::from_str(price_str).unwrap_or_default(),
                                market_id: event.get("market_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                timestamp_ms: event.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0),
                            };

                            let _ = self.tx.send(trade);
                        }
                    }
                }
            }
        }
    }
}
