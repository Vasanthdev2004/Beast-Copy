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

        // 1h Score refresh loop
        let tracker_for_refresh = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                info!("Refreshing wallet win rates from DB...");
                // In production, query Timescale DB here.
                // For now, simulate an update based on local trade counts.
                for mut entry in tracker_for_refresh.scores.iter_mut() {
                    let score = entry.value_mut();
                    if score.trade_count > 10 {
                        score.win_rate = 0.50 + (score.trade_count as f64 % 10.0) / 100.0;
                    }
                }
            }
        });
    }
}
