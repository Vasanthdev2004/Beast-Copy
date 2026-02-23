use alloy::primitives::Address;
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
    pub fn new(config: Arc<RwLock<AppConfig>>) -> Arc<Self> {
        let tracker = Arc::new(Self {
            scores: DashMap::new(),
            config: config.clone(),
        });

        let tracker_clone = tracker.clone();
        tokio::spawn(async move {
            let conf = tracker_clone.config.read().await;
            for target in &conf.wallets.targets {
                if let Ok(addr) = target.parse::<Address>() {
                    let score = WalletScore {
                        address: addr,
                        win_rate: 0.55, // Baseline
                        total_pnl: Decimal::ZERO,
                        trade_count: 0,
                        last_active: 0,
                    };
                    tracker_clone.scores.insert(addr, score);
                }
            }
        });

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
        tokio::spawn(async move {
            loop {
                let auto = self.config.read().await.wallets.auto_discover;
                if auto {
                    info!("Running wallet auto-discovery...");
                    // Placeholder for Polymarket Gamma API top 50 30-day ROI caller
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
    }
}
