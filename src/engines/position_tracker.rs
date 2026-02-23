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
    
    pub fn get_total_open_positions(&self) -> usize {
        self.positions.len()
    }
}
