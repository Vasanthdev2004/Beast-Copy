use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use notify::{Watcher, RecursiveMode, EventKind};
use std::path::Path;
use tracing::{info, error};

#[derive(Debug, Clone, Deserialize)]
pub struct WalletsConfig {
    pub targets: Vec<String>,
    pub auto_discover: bool,
    pub max_watched_wallets: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CopyConfig {
    pub preview_mode: bool,
    #[serde(default = "default_paper_balance")]
    pub paper_balance_usdc: f64,
    pub copy_ratio: f64,
    pub sizing_mode: String,
    pub fixed_usdc: f64,
    pub max_single_trade_usdc: f64,
    pub min_market_liquidity_usdc: f64,
    pub min_copy_size_usdc: f64,
    pub cooldown_per_market_secs: u64,
    pub dedup_window_secs: u64,
}

fn default_paper_balance() -> f64 { 1000.0 }

#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    pub daily_max_loss_usdc: f64,
    pub max_open_positions: usize,
    pub max_slippage_pct: f64,
    pub consecutive_loss_halt: usize,
    pub halt_duration_mins: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    pub polygon_rpc: String,
    pub backup_rpc: String,
    pub confirmation_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_user_ids: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub wallets: WalletsConfig,
    pub copy: CopyConfig,
    pub risk: RiskConfig,
    pub rpc: RpcConfig,
    pub telegram: TelegramConfig,
}

pub struct ConfigManager {
    pub config: Arc<RwLock<AppConfig>>,
}

impl ConfigManager {
    pub async fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_str().unwrap().to_string();
        let config_data = Self::load_config(&path_str)?;
        let arc_config = Arc::new(RwLock::new(config_data));
        
        Self::spawn_watcher(arc_config.clone(), path_str);
        
        Ok(Self { config: arc_config })
    }

    fn load_config(path: &str) -> Result<AppConfig> {
        let content = std::fs::read_to_string(path)?;
        let mut config: AppConfig = toml::from_str(&content)?;
        
        if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
            config.telegram.bot_token = token;
        }
        if let Ok(rpc) = std::env::var("POLYGON_RPC") {
            config.rpc.polygon_rpc = rpc;
        }
        // LIVE_TRADING=true in .env → preview_mode = false (real orders)
        // LIVE_TRADING=false in .env → preview_mode = true (paper trading)
        if let Ok(live) = std::env::var("LIVE_TRADING") {
            config.copy.preview_mode = live.to_lowercase() != "true";
        }
        
        Ok(config)
    }

    fn spawn_watcher(config: Arc<RwLock<AppConfig>>, path: String) {
        tokio::spawn(async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            
            let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    if matches!(event.kind, EventKind::Modify(_)) {
                        let _ = tx.send(());
                    }
                }
            }).expect("Failed to initialize watcher");
            
            watcher.watch(Path::new(&path), RecursiveMode::NonRecursive).expect("Failed to watch config");
            
            while let Some(_) = rx.recv().await {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                match Self::load_config(&path) {
                    Ok(new_config) => {
                        *config.write().await = new_config;
                        info!("Configuration hot-reloaded successfully");
                    }
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                    }
                }
            }
        });
    }
}
