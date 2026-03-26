use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, error};
use rust_decimal::Decimal;
use dashmap::DashMap;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashMap;

use crate::types::{OrderIntent, BotState};
use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;

pub struct RiskGate {
    intent_rx: mpsc::Receiver<OrderIntent>,
    clob_tx: mpsc::Sender<OrderIntent>,
    bot_state: Arc<RwLock<BotState>>,
    config: Arc<RwLock<AppConfig>>,
    position_tracker: Arc<PositionTracker>,
    consecutive_losses: Arc<AtomicUsize>,
    log_tx: tokio::sync::mpsc::UnboundedSender<crate::types::LogEntry>,
    daily_loss: Arc<RwLock<Decimal>>,
    market_cooldowns: Arc<RwLock<HashMap<String, u64>>>,
    halted_at: Arc<RwLock<Option<std::time::Instant>>>,
    /// Short-lived cache of market liquidity (USDC) to avoid per-trade RPC calls.
    /// Key: market_id, Value: (liquidity_usdc, cached_at_timestamp_secs)
    liquidity_cache: Arc<DashMap<String, (Decimal, u64)>>,
}

impl RiskGate {
    pub fn new(
        intent_rx: mpsc::Receiver<OrderIntent>,
        clob_tx: mpsc::Sender<OrderIntent>,
        bot_state: Arc<RwLock<BotState>>,
        config: Arc<RwLock<AppConfig>>,
        position_tracker: Arc<PositionTracker>,
        consecutive_losses: Arc<AtomicUsize>,
        log_tx: tokio::sync::mpsc::UnboundedSender<crate::types::LogEntry>,
        daily_loss: Arc<RwLock<Decimal>>,
    ) -> Self {
        Self {
            intent_rx,
            clob_tx,
            bot_state,
            config,
            position_tracker,
            consecutive_losses,
            log_tx,
            daily_loss,
            market_cooldowns: Arc::new(RwLock::new(HashMap::new())),
            halted_at: Arc::new(RwLock::new(None)),
            liquidity_cache: Arc::new(DashMap::new()),
        }
    }

    pub async fn run(mut self) {
        info!("RiskGate started");

        // Spawn daily loss reset task (resets at midnight UTC)
        let daily_loss_clone = self.daily_loss.clone();
        tokio::spawn(async move {
            loop {
                let now = chrono::Utc::now();
                let tomorrow = (now + chrono::Duration::days(1)).date_naive().and_hms_opt(0, 0, 0).unwrap();
                let until_midnight = tomorrow.and_utc().signed_duration_since(now);
                let secs = until_midnight.num_seconds().max(1) as u64;
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                *daily_loss_clone.write().await = Decimal::ZERO;
                info!("Daily loss counter reset at midnight UTC");
            }
        });

        while let Some(intent) = self.intent_rx.recv().await {
            // Auto-resume from halt after halt_duration_mins
            {
                let halted = self.halted_at.read().await;
                if let Some(halted_time) = *halted {
                    let halt_mins = self.config.read().await.risk.halt_duration_mins;
                    if halted_time.elapsed().as_secs() >= halt_mins * 60 {
                        drop(halted);
                        *self.halted_at.write().await = None;
                        *self.bot_state.write().await = BotState::Running;
                        self.consecutive_losses.store(0, Ordering::SeqCst);
                        let _ = self.log_tx.send(crate::types::LogEntry {
                            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                            kind: "RISK".to_string(),
                            message: format!("Auto-resumed after {}m halt", halt_mins),
                        });
                    }
                }
            }

            if self.pre_trade_checks(&intent).await {
                if let Err(e) = self.clob_tx.send(intent).await {
                    error!("RiskGate failed to send to CLOB executor: {}", e);
                }
            }
        }
    }

    async fn pre_trade_checks(&self, intent: &OrderIntent) -> bool {
        // 0. Sells/liquidations pass through unconditionally — must always be able to close losing positions
        if intent.is_sell {
            let _ = self.log_tx.send(crate::types::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "RISK".to_string(),
                message: "LIQUIDATION passed through risk gate".to_string(),
            });
            return true;
        }

        // 1. Bot state check
        if *self.bot_state.read().await != BotState::Running {
            self.log_skip("Bot paused/halted, skipped").await;
            return false;
        }

        let config = self.config.read().await;

        // 2. Min trade size (compare notional USDC value, not raw shares)
        let min_size = Decimal::from_f64_retain(config.copy.min_copy_size_usdc).unwrap_or(rust_decimal_macros::dec!(2.0));
        let notional_usdc = intent.size * intent.price;
        if notional_usdc < min_size {
            self.log_skip(&format!("Notional ${:.2} below min ${:.2}", notional_usdc, min_size)).await;
            return false;
        }

        // 3. Consecutive loss halt
        let losses = self.consecutive_losses.load(Ordering::SeqCst);
        if losses >= config.risk.consecutive_loss_halt {
            *self.halted_at.write().await = Some(std::time::Instant::now());
            *self.bot_state.write().await = BotState::Halted;
            let _ = self.log_tx.send(crate::types::LogEntry {
                time: chrono::Utc::now().format("%H:%M:%S").to_string(),
                kind: "RISK".to_string(),
                message: format!("HALTED: {} consecutive losses (auto-resume in {}m)", losses, config.risk.halt_duration_mins),
            });
            return false;
        }

        // 4. Max open positions
        let open_positions = self.position_tracker.get_total_open_positions();
        if open_positions >= config.risk.max_open_positions {
            self.log_skip(&format!("Max positions ({}) reached", open_positions)).await;
            return false;
        }

        // 5. Price bounds (safety — reject extreme prices)
        if intent.price < rust_decimal_macros::dec!(0.01) || intent.price > rust_decimal_macros::dec!(0.99) {
            self.log_skip(&format!("Price {} outside 0.01-0.99 bounds", intent.price)).await;
            return false;
        }

        // 6. Daily loss limit
        let daily_loss = *self.daily_loss.read().await;
        let max_daily = Decimal::from_f64_retain(config.risk.daily_max_loss_usdc).unwrap_or(rust_decimal_macros::dec!(300.0));
        if daily_loss >= max_daily {
            self.log_skip(&format!("Daily loss ${:.2} >= limit ${:.2}", daily_loss, max_daily)).await;
            return false;
        }

        // 7. Slippage check — price must be within configured slippage tolerance
        let slippage_pct = Decimal::from_f64_retain(config.risk.max_slippage_pct).unwrap_or(rust_decimal_macros::dec!(2.5));
        let max_price = rust_decimal_macros::dec!(1.0) - (slippage_pct / rust_decimal_macros::dec!(100.0));
        let min_price = slippage_pct / rust_decimal_macros::dec!(100.0);
        if intent.price > max_price || intent.price < min_price {
            self.log_skip(&format!("Price {:.2} outside safe band ({:.2}-{:.2})", intent.price, min_price, max_price)).await;
            return false;
        }

        // 8. Market cooldown
        let now_secs = chrono::Utc::now().timestamp() as u64;
        {
            let cooldowns = self.market_cooldowns.read().await;
            if let Some(&last_trade) = cooldowns.get(&intent.market_id) {
                if now_secs - last_trade < config.copy.cooldown_per_market_secs {
                    self.log_skip(&format!("Market {} in cooldown", &intent.market_id[..8.min(intent.market_id.len())])).await;
                    return false;
                }
            }
        }
        self.market_cooldowns.write().await.insert(intent.market_id.clone(), now_secs);

        // 9. Market liquidity check — skip if market doesn't have enough depth
        let min_liq = Decimal::from_f64_retain(config.copy.min_market_liquidity_usdc).unwrap_or(rust_decimal_macros::dec!(500.0));
        if min_liq > Decimal::ZERO {
            let now_ts = chrono::Utc::now().timestamp() as u64;
            let cached = self.liquidity_cache.get(&intent.market_id);

            let liquidity = if let Some(entry) = cached {
                let (liq, ts) = entry.value();
                if now_ts - *ts < 60 {
                    *liq
                } else {
                    Decimal::MIN
                }
            } else {
                Decimal::MIN
            };

            if liquidity < min_liq && liquidity != Decimal::MIN {
                self.log_skip(&format!("Market {} liquidity ${:.2} below min ${:.2}", &intent.market_id[..8.min(intent.market_id.len())], liquidity, min_liq)).await;
                return false;
            }

            // If not cached or stale, fetch liquidity from Polymarket CLOB API
            if liquidity == Decimal::MIN {
                let client = reqwest::Client::new();
                let url = format!("https://clob.polymarket.com/markets/{}", intent.market_id);
                if let Ok(res) = client.get(&url).send().await {
                    if let Ok(json) = res.json::<serde_json::Value>().await {
                        if let Some(liq) = json.get("liquidity")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<f64>().ok())
                        {
                            let liq_dec = Decimal::from_f64_retain(liq).unwrap_or(Decimal::ZERO);
                            self.liquidity_cache.insert(intent.market_id.clone(), (liq_dec, now_ts));
                            if liq_dec < min_liq {
                                self.log_skip(&format!("Market {} liquidity ${:.2} below min ${:.2}", &intent.market_id[..8.min(intent.market_id.len())], liq_dec, min_liq)).await;
                                return false;
                            }
                        }
                    }
                }
            }
        }

        true
    }

    async fn log_skip(&self, msg: &str) {
        let _ = self.log_tx.send(crate::types::LogEntry {
            time: chrono::Utc::now().format("%H:%M:%S").to_string(),
            kind: "SKIP".to_string(),
            message: msg.to_string(),
        });
    }
}
