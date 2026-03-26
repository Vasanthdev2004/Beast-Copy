pub mod config;
pub mod engines;
pub mod execution;
pub mod risk;
pub mod storage;
pub mod telegram;
pub mod api;
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
    if let Err(e) = dotenvy::dotenv() {
        tracing::warn!("No .env file: {}", e);
    }

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
    // Any -> API Dashboard
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel::<crate::types::LogEntry>();

    // 5. Instantiate Modules
    
    // RTDS Engine (reads wallets dynamically from WalletTracker)
    let rtds_engine = engines::rtds_engine::RtdsEngine::new(trade_tx, wallet_tracker.clone(), log_tx.clone());

    // Copy Engine
    let copy_engine = engines::copy_engine::CopyEngine::new(
        trade_rx,
        intent_tx,
        wallet_tracker.clone(),
        config.clone(),
        bot_state.clone(),
        usdc_balance.clone(),
        position_tracker.clone(),
        log_tx.clone(),
    );

    // Shared daily loss counter — written by ClobExecutor (live loss),
    // fed to RiskGate for daily-loss-limit check, and to SettlementMonitor for display.
    let daily_loss: Arc<RwLock<Decimal>> = Arc::new(RwLock::new(Decimal::ZERO));

    // Loss reporting channel: ClobExecutor → SettlementMonitor
    let (loss_tx, loss_rx) = tokio::sync::mpsc::unbounded_channel::<Decimal>();

    // Risk Gate
    let risk_gate = risk::risk_gate::RiskGate::new(
        intent_rx,
        clob_tx,
        bot_state.clone(),
        config.clone(),
        position_tracker.clone(),
        consecutive_losses.clone(),
        log_tx.clone(),
        daily_loss.clone(),
    );

    // 6. DB Initialization & State Recovery
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/poly_apex".to_string());
    
    let db_client_arc = if let Ok(client) = storage::db::DbClient::new(&db_url).await {
        // Recover previous state
        if let Ok(positions) = client.load_active_positions().await {
            for pos in positions {
                position_tracker.add_position(pos);
            }
        }
        Some(Arc::new(client))
    } else {
        tracing::warn!("Failed to connect to database. Running without persistent storage.");
        None
    };

    // CLOB Executor
    let clob_executor = execution::clob_executor::ClobExecutor::new(
        clob_rx,
        result_tx,
        config.clone(),
        log_tx.clone(),
        position_tracker.clone(),
        usdc_balance.clone(),
        db_client_arc.clone(),
        loss_tx.clone(),
    ).await;

    // Settlement Monitor
    let settlement_monitor = execution::settlement::SettlementMonitor::new(
        result_rx,
        position_tracker.clone(),
        config.clone(),
        consecutive_losses.clone(),
        log_tx.clone(),
        usdc_balance.clone(),
        daily_loss.clone(),
        loss_rx,
    );

    // 7. Database Async Flush (Every 30s)
    if let Some(db_client) = db_client_arc {
        let wt = wallet_tracker.clone();
        let pt = position_tracker.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                
                // Flush positions
                for entry in pt.positions.iter() {
                    if let Err(e) = db_client.flush_position(entry.value()).await {
                        tracing::warn!("DB flush position error: {}", e);
                    }
                }
                
                // Flush wallet scores
                for entry in wt.scores.iter() {
                    if let Err(e) = db_client.flush_wallet_score(entry.value()).await {
                        tracing::warn!("DB flush wallet score error: {}", e);
                    }
                }
            }
        });
        info!("TimescaleDB async flush enabled.");
    }

    // PnL Estimation Loop (updates position P&L estimates every 15s)
    {
        let pnl_pt = position_tracker.clone();
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                
                let keys: Vec<String> = pnl_pt.positions.iter().map(|e| e.key().clone()).collect();
                for key in keys {
                    if let Some(mut pos) = pnl_pt.positions.get_mut(&key) {
                        let shares = if pos.entry_price > rust_decimal_macros::dec!(0.0) {
                            pos.size / pos.entry_price
                        } else {
                            pos.size
                        };
                        
                        let url = format!("https://clob.polymarket.com/markets/{}", pos.market_id);
                        if let Ok(res) = client.get(&url).send().await {
                            if let Ok(json) = res.json::<serde_json::Value>().await {
                                if let Some(tokens) = json.get("tokens").and_then(|t| t.as_array()) {
                                    let target_outcome = match pos.side { crate::types::Side::Yes => "Yes", crate::types::Side::No => "No" };
                                    for t in tokens {
                                        if t.get("outcome").and_then(|v| v.as_str()).unwrap_or("") == target_outcome {
                                            if let Some(price_f64) = t.get("price").and_then(|v| v.as_f64()) {
                                                if let Some(current_price) = Decimal::from_f64_retain(price_f64) {
                                                    let estimated_pnl = (shares * current_price) - pos.size;
                                                    pos.pnl = Some(estimated_pnl.round_dp(2));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // 7. Spawn all components as Tokio Tasks (collect handles for monitoring)
    let mut engine_handles: Vec<(&str, tokio::task::JoinHandle<()>)> = Vec::new();
    engine_handles.push(("RtdsEngine", tokio::spawn(async move { rtds_engine.run().await; })));
    engine_handles.push(("CopyEngine", tokio::spawn(async move { copy_engine.run().await; })));
    engine_handles.push(("RiskGate", tokio::spawn(async move { risk_gate.run().await; })));
    engine_handles.push(("ClobExecutor", tokio::spawn(async move { clob_executor.run().await; })));
    engine_handles.push(("SettlementMonitor", tokio::spawn(async move { settlement_monitor.run().await; })));

    // Monitor engine tasks — log if any exit unexpectedly
    tokio::spawn(async move {
        let mut handles = engine_handles;
        while !handles.is_empty() {
            let mut exited = Vec::new();
            for (i, (name, handle)) in handles.iter_mut().enumerate() {
                if handle.is_finished() {
                    match handle.await {
                        Ok(()) => tracing::error!("Engine task '{}' exited unexpectedly", name),
                        Err(e) => tracing::error!("Engine task '{}' panicked: {}", name, e),
                    }
                    exited.push(i);
                }
            }
            // Remove exited handles in reverse order to preserve indices
            for i in exited.into_iter().rev() {
                handles.remove(i);
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    });

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

    // 9. Launch API Web Server Dashboard (runs on main thread)
    let app_state = Arc::new(api::server::AppState::new(
        config.clone(),
        bot_state.clone(),
        wallet_tracker.clone(),
        position_tracker.clone(),
        consecutive_losses,
        usdc_balance,
        initial_balance,
        daily_loss.clone(),
    ));

    api::server::run_server(app_state, log_rx).await;

    // Graceful shutdown: halt the pipeline so no new orders are submitted
    info!("Web UI server exited — initiating graceful shutdown...");
    *bot_state.write().await = BotState::Halted;
    
    // Give in-flight orders time to settle (2 seconds grace period)
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    info!("Shutdown complete.");

    Ok(())
}
