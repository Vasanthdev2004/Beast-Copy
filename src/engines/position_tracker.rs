use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::types::Position;
use crate::config::AppConfig;

pub struct PositionTracker {
    pub positions: DashMap<String, Position>,
    _config: Arc<RwLock<AppConfig>>,
}

impl PositionTracker {
    pub fn new(config: Arc<RwLock<AppConfig>>) -> Arc<Self> {
        Arc::new(Self {
            positions: DashMap::new(),
            _config: config,
        })
    }

    pub fn add_position(&self, position: Position) {
        let key = format!("{}-{:?}", position.market_id, position.side);
        info!("Tracking new position: {}", key);
        self.positions.insert(key, position);
    }

    pub fn close_position(&self, market_id: &str, side: &crate::types::Side) -> Option<Position> {
        let key = format!("{}-{:?}", market_id, side);
        self.positions.remove(&key).map(|(_, p)| {
            info!("Closed position: {}", key);
            p
        })
    }

    pub fn remove_position(&self, key: &str) -> Option<Position> {
        self.positions.remove(key).map(|(_, p)| {
            info!("Removed position: {}", key);
            p
        })
    }
    
    pub fn get_total_open_positions(&self) -> usize {
        self.positions.len()
    }

    pub fn get_total_unrealized_pnl(&self) -> rust_decimal::Decimal {
        self.positions.iter()
            .filter_map(|e| e.value().pnl)
            .sum()
    }
}
