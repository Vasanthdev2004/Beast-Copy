pub mod config;
pub mod engines;
pub mod execution;
pub mod risk;
pub mod storage;
pub mod telegram;
pub mod types;
pub mod utils;

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::info;

use crate::types::{BotState, TradeEvent, OrderIntent, OrderResult};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize Logging
    utils::logger::init();
    info!("POLY-APEX starting up...");

    // 2. Load Config
    let config_manager = config::ConfigManager::new("config.toml").await?;
    let config = config_manager.config.clone();

    // 3. Shared State
    let bot_state = Arc::new(RwLock::new(BotState::Running));
    let wallet_tracker = engines::wallet_tracker::WalletTracker::new(config.clone());
    let position_tracker = engines::position_tracker::PositionTracker::new(config.clone());

    // 4. Channels (Event Bus)
    // RTDS -> CopyEngine
    let (trade_tx, trade_rx) = broadcast::channel::<TradeEvent>(1000);
    // CopyEngine -> RiskGate
    let (intent_tx, intent_rx) = mpsc::channel::<OrderIntent>(1000);
    // RiskGate -> ClobExecutor
    let (clob_tx, clob_rx) = mpsc::channel::<OrderIntent>(1000);
    // ClobExecutor -> SettlementMonitor
    let (result_tx, result_rx) = mpsc::channel::<OrderResult>(1000);

    // 5. Instantiate Modules
    
    // RTDS Engine
    let rtds_wallets = wallet_tracker.scores.iter().map(|kv| *kv.key()).collect();
    let rtds_engine = engines::rtds_engine::RtdsEngine::new(trade_tx, rtds_wallets);

    // Copy Engine
    let copy_engine = engines::copy_engine::CopyEngine::new(
        trade_rx,
        intent_tx,
        wallet_tracker.clone(),
        config.clone(),
        bot_state.clone(),
    );

    // Risk Gate
    let risk_gate = risk::risk_gate::RiskGate::new(
        intent_rx,
        clob_tx,
        bot_state.clone(),
        config.clone(),
    );

    // CLOB Executor
    let clob_executor = execution::clob_executor::ClobExecutor::new(
        clob_rx,
        result_tx,
        config.clone(),
    );

    // Settlement Monitor
    let settlement_monitor = execution::settlement::SettlementMonitor::new(
        result_rx,
        position_tracker.clone(),
        config.clone(),
    );

    // 6. Spawn all components as Tokio Tasks
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
    tokio::spawn(async move {
        telegram::bot::start_bot(
            config.clone(),
            bot_state.clone(),
            wallet_tracker,
            position_tracker,
        ).await;
    });

    // Await forever
    let () = std::future::pending().await;

    Ok(())
}
