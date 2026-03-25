use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use rust_decimal::Decimal;

use crate::config::AppConfig;
use crate::types::BotState;
use crate::engines::wallet_tracker::WalletTracker;
use crate::engines::position_tracker::PositionTracker;

/// Lightweight Telegram Bot API client using reqwest (no teloxide dependency)
struct TgBot {
    client: reqwest::Client,
    base_url: String,
}

impl TgBot {
    fn new(token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("https://api.telegram.org/bot{}", token),
        }
    }

    async fn send(&self, chat_id: i64, text: &str, html: bool) {
        let mut params = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if html {
            params["parse_mode"] = serde_json::json!("HTML");
        }
        let url = format!("{}/sendMessage", self.base_url);
        if let Err(e) = self.client.post(&url).json(&params).send().await {
            error!("Telegram send error: {}", e);
        }
    }

    async fn poll_updates(&self, offset: &mut i64) -> Vec<serde_json::Value> {
        let url = format!("{}/getUpdates", self.base_url);
        let params = serde_json::json!({
            "offset": *offset,
            "timeout": 30,
            "allowed_updates": ["message"],
        });
        match self.client.post(&url).json(&params).send().await {
            Ok(res) => {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(updates) = json.get("result").and_then(|r| r.as_array()) {
                        for u in updates {
                            if let Some(id) = u.get("update_id").and_then(|i| i.as_i64()) {
                                *offset = id + 1;
                            }
                        }
                        return updates.clone();
                    }
                }
                vec![]
            }
            Err(e) => {
                warn!("Telegram poll error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                vec![]
            }
        }
    }
}

pub async fn start_bot(
    config: Arc<RwLock<AppConfig>>,
    bot_state: Arc<RwLock<BotState>>,
    wallet_tracker: Arc<WalletTracker>,
    position_tracker: Arc<PositionTracker>,
    usdc_balance: Arc<RwLock<Decimal>>,
    initial_balance: Decimal,
    start_time: std::time::Instant,
) {
    let token = config.read().await.telegram.bot_token.clone();
    if token.is_empty() {
        warn!("Telegram bot token empty, skipping bot startup");
        return;
    }

    let bot = TgBot::new(&token);
    info!("Starting Telegram bot (lightweight mode)...");

    let mut offset: i64 = 0;

    loop {
        let updates = bot.poll_updates(&mut offset).await;

        for update in updates {
            let chat_id = update.pointer("/message/chat/id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let text = update.pointer("/message/text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            
            if chat_id == 0 || text.is_empty() { continue; }

            // Auth check
            let allowed = config.read().await.telegram.allowed_user_ids.clone();
            if !allowed.contains(&(chat_id as u64)) {
                warn!("Unauthorized Telegram access from ID: {}", chat_id);
                continue;
            }

            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.is_empty() { continue; }

            match parts[0] {
                "/start" | "/help" => {
                    let help = concat!(
                        "🤖 <b>POLY-APEX Copy Trader</b>\n\n",
                        "📋 <b>Commands:</b>\n",
                        "🔹 /status — Bot state & uptime\n",
                        "🔹 /positions — View open trades\n",
                        "🔹 /balance — Current portfolio balance\n",
                        "🔹 /pnl — Realized & Unrealized P&L\n",
                        "🔹 /wallets — List tracked whales\n",
                        "🔹 <code>/addwallet 0x...</code> — Track a whale\n",
                        "🔹 <code>/removewallet 0x...</code> — Stop tracking\n",
                        "🔹 /risk — View risk limits\n",
                        "🔹 /pause — Pause copy trading\n",
                        "🔹 /resume — Resume copy trading\n",
                    );
                    bot.send(chat_id, help, true).await;
                },
                "/pause" => {
                    *bot_state.write().await = BotState::Paused;
                    bot.send(chat_id, "<b>Bot Paused</b> ⏸️", true).await;
                },
                "/resume" => {
                    *bot_state.write().await = BotState::Running;
                    bot.send(chat_id, "<b>Bot Resumed</b> ▶️", true).await;
                },
                "/status" => {
                    let s = *bot_state.read().await;
                    let cfg = config.read().await;
                    let is_preview = cfg.copy.preview_mode;
                    
                    let mode_str = if is_preview { "📄 PAPER" } else { "🔴 LIVE" };
                    let state_str = match s {
                        BotState::Running => "🟢 RUNNING",
                        BotState::Paused => "🟡 PAUSED",
                        BotState::Halted => "🛑 HALTED",
                    };
                    
                    let elapsed = start_time.elapsed();
                    let hours = elapsed.as_secs() / 3600;
                    let mins = (elapsed.as_secs() % 3600) / 60;
                    let uptime = if hours > 0 { format!("{}h {}m", hours, mins) } else { format!("{}m", mins) };
                    
                    let open_pos = position_tracker.get_total_open_positions();
                    let tracked = wallet_tracker.scores.len();
                    
                    let response = format!(
                        "📊 <b>Bot Status</b>\n\n\
                        <b>Mode:</b> {}\n\
                        <b>State:</b> {}\n\
                        <b>Uptime:</b> ⏱ {}\n\
                        <b>Open Positions:</b> {}\n\
                        <b>Tracked Wallets:</b> {}",
                        mode_str, state_str, uptime, open_pos, tracked
                    );
                    bot.send(chat_id, &response, true).await;
                },
                "/positions" => {
                    let positions: Vec<_> = position_tracker.positions.iter().map(|e| e.value().clone()).collect();
                    if positions.is_empty() {
                        bot.send(chat_id, "📭 No open positions.", false).await;
                        continue;
                    }
                    
                    let mut msg_text = format!("📋 <b>Open Positions ({})</b>\n\n", positions.len());
                    for p in positions {
                        let side = match p.side { crate::types::Side::Yes => "YES", crate::types::Side::No => "NO" };
                        let side_color = match p.side { crate::types::Side::Yes => "🟢", crate::types::Side::No => "🔴" };
                        
                        let pnl_str = if let Some(pnl) = p.pnl {
                            let sign = if pnl >= Decimal::ZERO { "+" } else { "" };
                            format!("{}${:.2}", sign, pnl.round_dp(2))
                        } else {
                            "—".to_string()
                        };
                        
                        let short_title = if p.market_id.len() > 25 {
                            format!("{}…", &p.market_id[..25])
                        } else {
                            p.market_id.clone()
                        };
                        
                        msg_text.push_str(&format!("{} <b>{}</b> ${:.2} @ ¢{} on <i>{}</i> [{}]\n",
                            side_color, side, p.size.round_dp(2), 
                            (p.entry_price * Decimal::from(100)).round_dp(0),
                            short_title, pnl_str
                        ));
                    }
                    
                    bot.send(chat_id, &msg_text, true).await;
                },
                "/balance" => {
                    let bal = *usdc_balance.read().await;
                    let response = format!("💰 <b>Current Balance:</b>\n<code>${:.2}</code> USDC", bal.round_dp(2));
                    bot.send(chat_id, &response, true).await;
                },
                "/pnl" => {
                    let bal = *usdc_balance.read().await;
                    let realized = bal - initial_balance;
                    
                    let positions: Vec<_> = position_tracker.positions.iter().map(|e| e.value().clone()).collect();
                    let unrealized: Decimal = positions.iter().filter_map(|p| p.pnl).sum();
                    
                    let total = realized + unrealized;
                    
                    let r_sign = if realized >= Decimal::ZERO { "+" } else { "" };
                    let u_sign = if unrealized >= Decimal::ZERO { "+" } else { "" };
                    let t_sign = if total >= Decimal::ZERO { "+" } else { "" };
                    
                    let response = format!(
                        "📈 <b>Session P&L</b>\n\n\
                        <b>Total P&L:</b> {}${:.2}\n\
                        <b>Realized:</b>  {}${:.2}\n\
                        <b>Unrealized:</b> {}${:.2}",
                        t_sign, total.round_dp(2),
                        r_sign, realized.round_dp(2),
                        u_sign, unrealized.round_dp(2)
                    );
                    bot.send(chat_id, &response, true).await;
                },
                "/wallets" => {
                    let count = wallet_tracker.scores.len();
                    if count == 0 {
                        bot.send(chat_id, "📭 No wallets tracked.", false).await;
                        continue;
                    }

                    let mut wallet_list = format!("🐋 <b>Target Whales ({})</b>\n\n", count);
                    for entry in wallet_tracker.scores.iter().take(10) {
                        let addr = format!("{:?}", entry.key());
                        let short = format!("0x{}…{}", &addr[2..6], &addr[38..42]);
                        let score = entry.value();
                        
                        wallet_list.push_str(&format!(
                            "🔹 <a href=\"https://polygonscan.com/address/{}\">{}</a>\n   └ {:.1}% WR │ {} Trades │ ${:.2} PnL\n\n",
                            addr, short, score.win_rate, score.trade_count, score.total_pnl.round_dp(2)
                        ));
                    }
                    bot.send(chat_id, &wallet_list, true).await;
                },
                "/addwallet" => {
                    if parts.len() > 1 {
                        if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                            wallet_tracker.add_wallet(addr);
                            bot.send(chat_id, &format!("✅ <b>Added Wallet:</b>\n<code>{}</code>", parts[1]), true).await;
                        } else {
                            bot.send(chat_id, "❌ <b>Invalid address format.</b>\nMust be a valid 0x polygon address.", true).await;
                        }
                    } else {
                        bot.send(chat_id, "ℹ️ <b>Usage:</b>\n<code>/addwallet 0x...</code>", true).await;
                    }
                },
                "/removewallet" => {
                    if parts.len() > 1 {
                        if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                            if wallet_tracker.remove_wallet(&addr) {
                                bot.send(chat_id, &format!("✅ <b>Removed Wallet:</b>\n<code>{}</code>", parts[1]), true).await;
                            } else {
                                bot.send(chat_id, "❌ <b>Wallet not found in tracking list.</b>", true).await;
                            }
                        } else {
                            bot.send(chat_id, "❌ <b>Invalid address format.</b>", true).await;
                        }
                    } else {
                        bot.send(chat_id, "ℹ️ <b>Usage:</b>\n<code>/removewallet 0x...</code>", true).await;
                    }
                },
                "/risk" => {
                    let r = config.read().await.risk.clone();
                    let msg_text = format!(
                        "🛡️ <b>Risk Limits</b>\n\n\
                        <b>Max Daily Loss:</b> ${}\n\
                        <b>Max Positions:</b> {}\n\
                        <b>Max Slippage:</b> {}%\n\
                        <b>Loss Halt:</b> {} consecutive", 
                        r.daily_max_loss_usdc, r.max_open_positions, r.max_slippage_pct, r.consecutive_loss_halt
                    );
                    bot.send(chat_id, &msg_text, true).await;
                },
                _ => {
                    bot.send(chat_id, "❓ Unknown command. Type /help to see the menu.", false).await;
                }
            }
        }
    }
}
