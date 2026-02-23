use teloxide::prelude::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use rust_decimal::Decimal;

use crate::config::AppConfig;
use crate::types::BotState;
use crate::engines::wallet_tracker::WalletTracker;
use crate::engines::position_tracker::PositionTracker;

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

    let bot = Bot::new(token);
    info!("Starting Telegram bot...");
    
    let cloned_state = bot_state.clone();
    let cloned_config = config.clone();
    let cloned_wallets = wallet_tracker.clone();
    let cloned_positions = position_tracker.clone();
    let cloned_balance = usdc_balance.clone();
    
    teloxide::repl(bot, move |m_bot: Bot, msg: Message| {
        let state = cloned_state.clone();
        let conf = cloned_config.clone();
        let wt = cloned_wallets.clone();
        let pt = cloned_positions.clone();
        let balance_arc = cloned_balance.clone();
        
        async move {
            if let Some(text) = msg.text() {
                let allowed = conf.read().await.telegram.allowed_user_ids.clone();
                // Auth check
                if !allowed.contains(&(msg.chat.id.0 as u64)) {
                    warn!("Unauthorized Telegram access attempt from ID: {}", msg.chat.id);
                    return Ok(());
                }

                let parts: Vec<&str> = text.split_whitespace().collect();
                if parts.is_empty() { return Ok(()); }
                
                let mode_html = teloxide::types::ParseMode::Html;
                
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
                        let _ = m_bot.send_message(msg.chat.id, help).parse_mode(mode_html).await;
                    },
                    "/pause" => {
                        *state.write().await = BotState::Paused;
                        let _ = m_bot.send_message(msg.chat.id, "<b>Bot Paused</b> ⏸️").parse_mode(mode_html).await;
                    },
                    "/resume" => {
                        *state.write().await = BotState::Running;
                        let _ = m_bot.send_message(msg.chat.id, "<b>Bot Resumed</b> ▶️").parse_mode(mode_html).await;
                    },
                    "/status" => {
                        let s = *state.read().await;
                        let config = conf.read().await;
                        let is_preview = config.copy.preview_mode;
                        
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
                        
                        let open_pos = pt.get_total_open_positions();
                        let tracked = wt.scores.len();
                        
                        let response = format!(
                            "📊 <b>Bot Status</b>\n\n\
                            <b>Mode:</b> {}\n\
                            <b>State:</b> {}\n\
                            <b>Uptime:</b> ⏱ {}\n\
                            <b>Open Positions:</b> {}\n\
                            <b>Tracked Wallets:</b> {}",
                            mode_str, state_str, uptime, open_pos, tracked
                        );
                        let _ = m_bot.send_message(msg.chat.id, response).parse_mode(mode_html).await;
                    },
                    "/positions" => {
                        let positions: Vec<_> = pt.positions.iter().map(|e| e.value().clone()).collect();
                        if positions.is_empty() {
                            let _ = m_bot.send_message(msg.chat.id, "📭 No open positions.").await;
                            return Ok(());
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
                        
                        let _ = m_bot.send_message(msg.chat.id, msg_text).parse_mode(mode_html).await;
                    },
                    "/balance" => {
                        let bal = *balance_arc.read().await;
                        let response = format!("💰 <b>Current Balance:</b>\n<code>${:.2}</code> USDC", bal.round_dp(2));
                        let _ = m_bot.send_message(msg.chat.id, response).parse_mode(mode_html).await;
                    },
                    "/pnl" => {
                        let bal = *balance_arc.read().await;
                        let realized = bal - initial_balance;
                        
                        let positions: Vec<_> = pt.positions.iter().map(|e| e.value().clone()).collect();
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
                        let _ = m_bot.send_message(msg.chat.id, response).parse_mode(mode_html).await;
                    },
                    "/wallets" => {
                        let count = wt.scores.len();
                        if count == 0 {
                            let _ = m_bot.send_message(msg.chat.id, "📭 No wallets tracked.").await;
                            return Ok(());
                        }

                        let mut wallet_list = format!("🐋 <b>Target Whales ({})</b>\n\n", count);
                        for entry in wt.scores.iter().take(10) {
                            let addr = format!("{:?}", entry.key());
                            let short = format!("0x{}…{}", &addr[2..6], &addr[38..42]);
                            let score = entry.value();
                            
                            wallet_list.push_str(&format!(
                                "🔹 <a href=\"https://polygonscan.com/address/{}\">{}</a>\n   └ {:.1}% WR │ {} Trades │ ${:.2} PnL\n\n",
                                addr, short, score.win_rate, score.trade_count, score.total_pnl.round_dp(2)
                            ));
                        }
                        let _ = m_bot.send_message(msg.chat.id, wallet_list).parse_mode(mode_html).await;
                    },
                    "/addwallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                wt.add_wallet(addr);
                                let _ = m_bot.send_message(msg.chat.id, format!("✅ <b>Added Wallet:</b>\n<code>{}</code>", parts[1])).parse_mode(mode_html).await;
                            } else {
                                let _ = m_bot.send_message(msg.chat.id, "❌ <b>Invalid address format.</b>\nMust be a valid 0x polygon address.").parse_mode(mode_html).await;
                            }
                        } else {
                            let _ = m_bot.send_message(msg.chat.id, "ℹ️ <b>Usage:</b>\n<code>/addwallet 0x...</code>").parse_mode(mode_html).await;
                        }
                    },
                    "/removewallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                if wt.remove_wallet(&addr) {
                                    let _ = m_bot.send_message(msg.chat.id, format!("✅ <b>Removed Wallet:</b>\n<code>{}</code>", parts[1])).parse_mode(mode_html).await;
                                } else {
                                    let _ = m_bot.send_message(msg.chat.id, "❌ <b>Wallet not found in tracking list.</b>").parse_mode(mode_html).await;
                                }
                            } else {
                                let _ = m_bot.send_message(msg.chat.id, "❌ <b>Invalid address format.</b>").parse_mode(mode_html).await;
                            }
                        } else {
                            let _ = m_bot.send_message(msg.chat.id, "ℹ️ <b>Usage:</b>\n<code>/removewallet 0x...</code>").parse_mode(mode_html).await;
                        }
                    },
                    "/risk" => {
                        let r = conf.read().await.risk.clone();
                        let msg_text = format!(
                            "🛡️ <b>Risk Limits</b>\n\n\
                            <b>Max Daily Loss:</b> ${}\n\
                            <b>Max Positions:</b> {}\n\
                            <b>Max Slippage:</b> {}%\n\
                            <b>Loss Halt:</b> {} consecutive", 
                            r.daily_max_loss_usdc, r.max_open_positions, r.max_slippage_pct, r.consecutive_loss_halt
                        );
                        let _ = m_bot.send_message(msg.chat.id, msg_text).parse_mode(mode_html).await;
                    },
                    _ => {
                        let _ = m_bot.send_message(msg.chat.id, "❓ Unknown command. Type /help to see the menu.").await;
                    }
                }
            }
            Ok(())
        }
    })
    .await;
}
