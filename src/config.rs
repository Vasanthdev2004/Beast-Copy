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
        
        // ── Wallets ──
        if let Ok(v) = std::env::var("WALLET_TARGETS") {
            config.wallets.targets = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        if let Ok(v) = std::env::var("AUTO_DISCOVER") {
            config.wallets.auto_discover = v.to_lowercase() == "true";
        }
        if let Ok(v) = std::env::var("MAX_WATCHED_WALLETS") {
            if let Ok(n) = v.parse() { config.wallets.max_watched_wallets = n; }
        }
        
        // ── Copy ──
        if let Ok(v) = std::env::var("LIVE_TRADING") {
            config.copy.preview_mode = v.to_lowercase() != "true";
        }
        if let Ok(v) = std::env::var("PREVIEW_MODE") {
            config.copy.preview_mode = v.to_lowercase() == "true";
        }
        if let Ok(v) = std::env::var("PAPER_BALANCE_USDC") {
            if let Ok(n) = v.parse() { config.copy.paper_balance_usdc = n; }
        }
        if let Ok(v) = std::env::var("COPY_RATIO") {
            if let Ok(n) = v.parse() { config.copy.copy_ratio = n; }
        }
        if let Ok(v) = std::env::var("SIZING_MODE") {
            config.copy.sizing_mode = v;
        }
        if let Ok(v) = std::env::var("FIXED_USDC") {
            if let Ok(n) = v.parse() { config.copy.fixed_usdc = n; }
        }
        if let Ok(v) = std::env::var("MAX_SINGLE_TRADE_USDC") {
            if let Ok(n) = v.parse() { config.copy.max_single_trade_usdc = n; }
        }
        if let Ok(v) = std::env::var("MIN_MARKET_LIQUIDITY_USDC") {
            if let Ok(n) = v.parse() { config.copy.min_market_liquidity_usdc = n; }
        }
        if let Ok(v) = std::env::var("MIN_COPY_SIZE_USDC") {
            if let Ok(n) = v.parse() { config.copy.min_copy_size_usdc = n; }
        }
        if let Ok(v) = std::env::var("COOLDOWN_PER_MARKET_SECS") {
            if let Ok(n) = v.parse() { config.copy.cooldown_per_market_secs = n; }
        }
        if let Ok(v) = std::env::var("DEDUP_WINDOW_SECS") {
            if let Ok(n) = v.parse() { config.copy.dedup_window_secs = n; }
        }
        
        // ── Risk ──
        if let Ok(v) = std::env::var("DAILY_MAX_LOSS_USDC") {
            if let Ok(n) = v.parse() { config.risk.daily_max_loss_usdc = n; }
        }
        if let Ok(v) = std::env::var("MAX_OPEN_POSITIONS") {
            if let Ok(n) = v.parse() { config.risk.max_open_positions = n; }
        }
        if let Ok(v) = std::env::var("MAX_SLIPPAGE_PCT") {
            if let Ok(n) = v.parse() { config.risk.max_slippage_pct = n; }
        }
        if let Ok(v) = std::env::var("CONSECUTIVE_LOSS_HALT") {
            if let Ok(n) = v.parse() { config.risk.consecutive_loss_halt = n; }
        }
        if let Ok(v) = std::env::var("HALT_DURATION_MINS") {
            if let Ok(n) = v.parse() { config.risk.halt_duration_mins = n; }
        }
        
        // ── RPC ──
        if let Ok(v) = std::env::var("POLYGON_RPC") {
            config.rpc.polygon_rpc = v;
        }
        if let Ok(v) = std::env::var("BACKUP_RPC") {
            config.rpc.backup_rpc = v;
        }
        if let Ok(v) = std::env::var("CONFIRMATION_TIMEOUT_SECS") {
            if let Ok(n) = v.parse() { config.rpc.confirmation_timeout_secs = n; }
        }
        
        // ── Telegram ──
        if let Ok(v) = std::env::var("TELEGRAM_BOT_TOKEN") {
            config.telegram.bot_token = v;
        }
        if let Ok(v) = std::env::var("TELEGRAM_ALLOWED_IDS") {
            config.telegram.allowed_user_ids = v.split(',')
                .filter_map(|s| s.trim().parse::<u64>().ok())
                .collect();
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
