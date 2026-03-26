use alloy_primitives::Address;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;
use tokio::sync::RwLock;

use crate::types::WalletScore;
use crate::config::AppConfig;
use rust_decimal::Decimal;

pub struct WalletTracker {
    pub scores: DashMap<Address, WalletScore>,
    config: Arc<RwLock<AppConfig>>,
}

impl WalletTracker {
    pub async fn new(config: Arc<RwLock<AppConfig>>) -> Arc<Self> {
        let tracker = Arc::new(Self {
            scores: DashMap::new(),
            config: config.clone(),
        });

        let conf = config.read().await;
        for target in &conf.wallets.targets {
            if let Ok(addr) = target.parse::<Address>() {
                let score = WalletScore {
                    address: addr,
                    win_rate: 0.55, // Baseline
                    total_pnl: Decimal::ZERO,
                    trade_count: 0,
                    last_active: 0,
                };
                tracker.scores.insert(addr, score);
            }
        }

        tracker
    }

    pub fn add_wallet(&self, address: Address) {
        self.scores.entry(address).or_insert(WalletScore {
            address,
            win_rate: 0.55,
            total_pnl: Decimal::ZERO,
            trade_count: 0,
            last_active: 0,
        });
        info!("Added wallet to tracker: {}", address);
    }

    pub fn remove_wallet(&self, address: &Address) -> bool {
        let removed = self.scores.remove(address).is_some();
        if removed {
            info!("Removed wallet from tracker: {}", address);
        }
        removed
    }

    pub fn get_score(&self, address: &Address) -> Option<WalletScore> {
        self.scores.get(address).map(|r| r.clone())
    }

    pub fn is_watched(&self, address: &Address) -> bool {
        self.scores.contains_key(address)
    }

    pub fn start_auto_discovery(self: Arc<Self>) {
        let tracker_clone = self.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            loop {
                let auto = tracker_clone.config.read().await.wallets.auto_discover;
                if auto {
                    info!("Running wallet auto-discovery from Gamma API...");
                    if let Ok(res) = client.get("https://gamma-api.polymarket.com/leaderboard?limit=50&window=30d").send().await {
                        if let Ok(json) = res.json::<serde_json::Value>().await {
                            if let Some(arr) = json.as_array() {
                                let max = tracker_clone.config.read().await.wallets.max_watched_wallets;
                                let mut added = 0;
                                for item in arr {
                                    if added >= max { break; }
                                    if let Some(addr_str) = item.get("address").and_then(|v| v.as_str()) {
                                        if let Ok(addr) = addr_str.parse::<Address>() {
                                            if !tracker_clone.is_watched(&addr) {
                                                tracker_clone.add_wallet(addr);
                                                added += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });

        // 1h Score refresh loop — fetch immediately on boot, then every hour
        let tracker_for_refresh = self.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            // Immediate fetch so Kelly sizing has real data from second zero
            Self::refresh_scores(&tracker_for_refresh, &client).await;
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Self::refresh_scores(&tracker_for_refresh, &client).await;
            }
        });
    }

    async fn refresh_scores(tracker: &Arc<Self>, client: &reqwest::Client) {
        info!("Refreshing wallet win rates from Data API...");

        let addrs: Vec<Address> = tracker.scores.iter().map(|e| *e.key()).collect();

        for addr in addrs {
            let addr_str = format!("{:?}", addr);
            let url = format!(
                "https://data-api.polymarket.com/activity?user={}&limit=500&offset=0",
                addr_str
            );

            if let Ok(res) = client.get(&url).send().await {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(arr) = json.as_array() {
                        let mut markets: std::collections::HashMap<String, (f64, f64)> =
                            std::collections::HashMap::new();

                        for item in arr {
                            let c_id = item.get("conditionId").and_then(|v| v.as_str()).unwrap_or("");
                            if c_id.is_empty() { continue; }

                            let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            let usdc = item.get("usdcSize").and_then(|v| v.as_f64()).unwrap_or(0.0);

                            let entry = markets.entry(c_id.to_string()).or_insert((0.0, 0.0));

                            if ty == "TRADE" {
                                let side = item.get("side").and_then(|v| v.as_str()).unwrap_or("");
                                if side == "BUY" {
                                    entry.0 += usdc;
                                } else if side == "SELL" {
                                    entry.1 += usdc;
                                }
                            } else if ty == "REDEEM" || ty == "SETTLE" {
                                entry.1 += usdc;
                            }
                        }

                        let mut wins = 0;
                        let mut total = 0;
                        let mut total_pnl = 0.0;

                        for (_, (cost, rev)) in markets {
                            if rev > 0.0 || cost > 0.0 {
                                total += 1;
                                let pnl = rev - cost;
                                total_pnl += pnl;
                                if pnl > 0.0 {
                                    wins += 1;
                                }
                            }
                        }

                        let win_rate = if total > 2 { wins as f64 / total as f64 } else { 0.55 };

                        if let Some(mut score) = tracker.scores.get_mut(&addr) {
                            score.win_rate = win_rate;
                            score.total_pnl = Decimal::from_f64_retain(total_pnl).unwrap_or(Decimal::ZERO);
                            score.trade_count = total as u32;
                            info!("Wallet {} → win_rate={:.2}%, PnL=${:.2}, trades={}",
                                &addr_str[..10.min(addr_str.len())], win_rate * 100.0, total_pnl, total);
                        }
                    }
                }
            }
        }
    }
}
