use axum::{
    extract::State,
    routing::get,
    Router, Json,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use tower_http::cors::CorsLayer;
use rust_decimal::Decimal;
use serde::Serialize;
use tracing::info;

use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;
use crate::engines::wallet_tracker::WalletTracker;
use crate::types::{BotState, LogEntry, Position, WalletScore};

/// Shared dashboard state served via the API
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub bot_state: Arc<RwLock<BotState>>,
    pub wallet_tracker: Arc<WalletTracker>,
    pub position_tracker: Arc<PositionTracker>,
    pub consecutive_losses: Arc<std::sync::atomic::AtomicUsize>,
    pub trade_log: Arc<RwLock<Vec<LogEntry>>>,
    pub usdc_balance: Arc<RwLock<Decimal>>,
    pub initial_balance: Decimal,
    pub start_time: std::time::Instant,
    pub daily_loss: Arc<RwLock<Decimal>>,
}

impl AppState {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        bot_state: Arc<RwLock<BotState>>,
        wallet_tracker: Arc<WalletTracker>,
        position_tracker: Arc<PositionTracker>,
        consecutive_losses: Arc<std::sync::atomic::AtomicUsize>,
        usdc_balance: Arc<RwLock<Decimal>>,
        initial_balance: Decimal,
        daily_loss: Arc<RwLock<Decimal>>,
    ) -> Self {
        Self {
            config,
            bot_state,
            wallet_tracker,
            position_tracker,
            consecutive_losses,
            trade_log: Arc::new(RwLock::new(Vec::new())),
            usdc_balance,
            initial_balance,
            start_time: std::time::Instant::now(),
            daily_loss,
        }
    }
}

#[derive(Serialize)]
pub struct DashboardStateResponse {
    pub mode: String,
    pub state: String,
    pub uptime_secs: u64,
    pub current_balance: f64,
    pub initial_balance: f64,
    pub session_pnl: f64,
    pub positions: Vec<Position>,
    pub target_wallets: Vec<WalletScore>,
    pub losses: usize,
    pub halt_limit: usize,
    pub daily_loss: f64,
    pub daily_max_loss: f64,
    pub total_trades_session: usize,
    pub win_rate: f64,
    pub max_open_positions: usize,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_secs: u64,
}

/// Run the Axum web server
pub async fn run_server(
    state: Arc<AppState>,
    mut log_rx: tokio::sync::mpsc::UnboundedReceiver<LogEntry>,
) {
    // Background task to drain and store incoming logs
    let log_state = state.clone();
    tokio::spawn(async move {
        loop {
            if let Some(entry) = log_rx.recv().await {
                let mut logs = log_state.trade_log.write().await;
                logs.push(entry);
                if logs.len() > 200 {
                    let drain_to = logs.len() - 200;
                    logs.drain(0..drain_to);
                }
            }
        }
    });

    let shared_state = state.clone();

    // Setup typical Axum app with static file serving + CORS
    let app = Router::new()
        .route("/api/state", get(get_state))
        .route("/api/logs", get(get_logs))
        .route("/api/health", get(get_health))
        .fallback_service(ServeDir::new("public"))
        .layer(CorsLayer::permissive())
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    info!("🌐 Web Dashboard running on http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}

// Handler for the main state JSON
async fn get_state(State(state): State<Arc<AppState>>) -> Json<DashboardStateResponse> {
    let conf = state.config.read().await;
    let b_state = *state.bot_state.read().await;
    let balance = *state.usdc_balance.read().await;
    let initial = state.initial_balance;
    let losses = state.consecutive_losses.load(Ordering::Relaxed);
    let daily_loss_val = *state.daily_loss.read().await;
    
    let bot_state_str = match b_state {
        BotState::Running => "RUNNING",
        BotState::Paused => "PAUSED",
        BotState::Halted => "HALTED",
    };
    
    let session_pnl = balance - initial;

    let positions: Vec<Position> = state.position_tracker.positions.iter().map(|kv| kv.value().clone()).collect();
    let wallets: Vec<WalletScore> = state.wallet_tracker.scores.iter().map(|kv| kv.value().clone()).collect();

    // Count COPY + FILL log entries for total_trades_session
    let logs = state.trade_log.read().await;
    let total_trades_session = logs.iter()
        .filter(|l| l.kind == "COPY" || l.kind == "FILL")
        .count();

    // Compute average win_rate across all tracked wallets
    let win_rate = if wallets.is_empty() {
        0.0
    } else {
        let sum: f64 = wallets.iter().map(|w| w.win_rate).sum();
        sum / wallets.len() as f64
    };

    Json(DashboardStateResponse {
        mode: if conf.copy.preview_mode { "PAPER".into() } else { "LIVE".into() },
        state: bot_state_str.into(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        current_balance: rust_decimal::prelude::ToPrimitive::to_f64(&balance).unwrap_or(0.0),
        initial_balance: rust_decimal::prelude::ToPrimitive::to_f64(&initial).unwrap_or(0.0),
        session_pnl: rust_decimal::prelude::ToPrimitive::to_f64(&session_pnl).unwrap_or(0.0),
        positions,
        target_wallets: wallets,
        losses,
        halt_limit: conf.risk.consecutive_loss_halt,
        daily_loss: rust_decimal::prelude::ToPrimitive::to_f64(&daily_loss_val).unwrap_or(0.0),
        daily_max_loss: conf.risk.daily_max_loss_usdc,
        total_trades_session,
        win_rate,
        max_open_positions: conf.risk.max_open_positions,
    })
}

// Handler for the live feed logs
async fn get_logs(State(state): State<Arc<AppState>>) -> Json<Vec<LogEntry>> {
    let logs = state.trade_log.read().await.clone();
    Json(logs)
}

// Handler for health check
async fn get_health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        uptime_secs: state.start_time.elapsed().as_secs(),
    })
}
