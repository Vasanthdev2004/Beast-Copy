pub mod config;
pub mod engines;
pub mod execution;
pub mod risk;
pub mod storage;
pub mod telegram;
pub mod tui;
pub mod types;
pub mod utils;

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::info;

use crate::types::{BotState, TradeEvent, OrderIntent, OrderResult};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize Logging
    utils::logger::init();
    info!("POLY-APEX starting up...");

    // 1b. Load .env file
    let _ = dotenvy::dotenv();

    // 2. Load Config
    let config_manager = config::ConfigManager::new("config.toml").await?;
    let config = config_manager.config.clone();

    // 3. Shared State
    let bot_state = Arc::new(RwLock::new(BotState::Running));
    let consecutive_losses = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wallet_tracker = engines::wallet_tracker::WalletTracker::new(config.clone()).await;
    let position_tracker = engines::position_tracker::PositionTracker::new(config.clone());
    let initial_balance = Decimal::from_f64_retain(config.read().await.copy.paper_balance_usdc)
        .unwrap_or(dec!(1000));
    let usdc_balance = Arc::new(RwLock::new(initial_balance));

    // 4. Channels (Event Bus)
    // RTDS -> CopyEngine
    let (trade_tx, trade_rx) = broadcast::channel::<TradeEvent>(1000);
    // CopyEngine -> RiskGate
    let (intent_tx, intent_rx) = mpsc::channel::<OrderIntent>(1000);
    // RiskGate -> ClobExecutor
    let (clob_tx, clob_rx) = mpsc::channel::<OrderIntent>(1000);
    // ClobExecutor -> SettlementMonitor
    let (result_tx, result_rx) = mpsc::channel::<OrderResult>(1000);
    // Any -> TUI Dashboard
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel::<crate::tui::dashboard::LogEntry>();

    // 5. Instantiate Modules
    
    // RTDS Engine
    let rtds_wallets = wallet_tracker.scores.iter().map(|kv| *kv.key()).collect();
    let rtds_engine = engines::rtds_engine::RtdsEngine::new(trade_tx, rtds_wallets, log_tx.clone());

    // Copy Engine
    let copy_engine = engines::copy_engine::CopyEngine::new(
        trade_rx,
        intent_tx,
        wallet_tracker.clone(),
        config.clone(),
        bot_state.clone(),
        usdc_balance.clone(),
        log_tx.clone(),
    );

    // Risk Gate
    let risk_gate = risk::risk_gate::RiskGate::new(
        intent_rx,
        clob_tx,
        bot_state.clone(),
        config.clone(),
        position_tracker.clone(),
        consecutive_losses.clone(),
        log_tx.clone(),
    );

    // CLOB Executor
    let clob_executor = execution::clob_executor::ClobExecutor::new(
        clob_rx,
        result_tx,
        config.clone(),
        log_tx.clone(),
        position_tracker.clone(),
        usdc_balance.clone(),
    ).await;

    // Settlement Monitor
    let settlement_monitor = execution::settlement::SettlementMonitor::new(
        result_rx,
        position_tracker.clone(),
        config.clone(),
        consecutive_losses.clone(),
        log_tx.clone(),
    );

    // 6. Database Async Flush (Every 30s)
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/poly_apex".to_string());
    if let Ok(db_client) = storage::db::DbClient::new(&db_url).await {
        let db_client = Arc::new(db_client);
        let wt = wallet_tracker.clone();
        let pt = position_tracker.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                
                // Flush positions
                for entry in pt.positions.iter() {
                    let _ = db_client.flush_position(entry.value()).await;
                }
                
                // Flush wallet scores
                for entry in wt.scores.iter() {
                    let _ = db_client.flush_wallet_score(entry.value()).await;
                }
            }
        });
        info!("TimescaleDB async flush enabled.");
    } else {
        tracing::warn!("Failed to connect to database. Running without persistent storage.");
    }

    // 7. Spawn all components as Tokio Tasks
    tokio::spawn(async move { rtds_engine.run().await; });
    tokio::spawn(async move { copy_engine.run().await; });
    tokio::spawn(async move { risk_gate.run().await; });
    tokio::spawn(async move { clob_executor.run().await; });
    tokio::spawn(async move { settlement_monitor.run().await; });

    // Optional Auto Discovery
    if config.read().await.wallets.auto_discover {
        wallet_tracker.clone().start_auto_discovery();
    }

    // 7. Start Telegram Bot Control Center
    let tg_config = config.clone();
    let tg_bot_state = bot_state.clone();
    let tg_wt = wallet_tracker.clone();
    let tg_pt = position_tracker.clone();
    let tg_balance = usdc_balance.clone();
    let tg_initial_balance = initial_balance;
    let tg_start_time = std::time::Instant::now();
    tokio::spawn(async move {
        telegram::bot::start_bot(
            tg_config,
            tg_bot_state,
            tg_wt,
            tg_pt,
            tg_balance,
            tg_initial_balance,
            tg_start_time,
        ).await;
    });

    // 8. (Removed local usdc_balance, it was moved up to shared state)

    // 9. Launch TUI Dashboard (runs on main thread)
    let dashboard_state = Arc::new(tui::dashboard::DashboardState::new(
        config.clone(),
        bot_state.clone(),
        wallet_tracker.clone(),
        position_tracker.clone(),
        consecutive_losses,
        usdc_balance,
        initial_balance,
    ));

    tui::dashboard::run_dashboard(dashboard_state, log_rx).await.ok();

    // Graceful shutdown: halt the pipeline so no new orders are submitted
    info!("TUI exited — initiating graceful shutdown...");
    *bot_state.write().await = BotState::Halted;
    
    // Give in-flight orders time to settle (2 seconds grace period)
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    info!("Shutdown complete.");

    Ok(())
}
