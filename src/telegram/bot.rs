use teloxide::prelude::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::types::BotState;
use crate::engines::wallet_tracker::WalletTracker;
use crate::engines::position_tracker::PositionTracker;

pub async fn start_bot(
    config: Arc<RwLock<AppConfig>>,
    bot_state: Arc<RwLock<BotState>>,
    wallet_tracker: Arc<WalletTracker>,
    position_tracker: Arc<PositionTracker>,
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
    
    teloxide::repl(bot, move |m_bot: Bot, msg: Message| {
        let state = cloned_state.clone();
        let conf = cloned_config.clone();
        let wt = cloned_wallets.clone();
        let pt = cloned_positions.clone();
        
        async move {
            if let Some(text) = msg.text() {
                let allowed = conf.read().await.telegram.allowed_user_ids.clone();
                // Simple auth check
                if !allowed.contains(&(msg.chat.id.0 as u64)) {
                    warn!("Unauthorized Telegram access attempt from ID: {}", msg.chat.id);
                    return Ok(());
                }

                let parts: Vec<&str> = text.split_whitespace().collect();
                if parts.is_empty() { return Ok(()); }
                
                match parts[0] {
                    "/start" | "/help" => {
                        let help = "🤖 *POLY-APEX Copy Trader*\n\n\
                            📋 *Commands:*\n\
                            /status — Bot state & open positions\n\
                            /balance — Current portfolio balance\n\
                            /pnl — Session P&L\n\
                            /wallets — List tracked wallets\n\
                            /addwallet `<addr>` — Track a wallet\n\
                            /removewallet `<addr>` — Stop tracking\n\
                            /risk — View risk limits\n\
                            /pause — Pause copy trading\n\
                            /resume — Resume copy trading\n\
                            /help — Show this menu";
                        let _ = m_bot.send_message(msg.chat.id, help)
                            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                            .await;
                    },
                    "/pause" => {
                        *state.write().await = BotState::Paused;
                        let _ = m_bot.send_message(msg.chat.id, "Bot Paused ⏸️").await;
                    },
                    "/resume" => {
                        *state.write().await = BotState::Running;
                        let _ = m_bot.send_message(msg.chat.id, "Bot Resumed ▶️").await;
                    },
                    "/status" => {
                        let s = *state.read().await;
                        let open_pos = pt.get_total_open_positions();
                        let response = format!("🤖 Bot State: {:?}\n📈 Open Positions: {}", s, open_pos);
                        let _ = m_bot.send_message(msg.chat.id, response).await;
                    },
                    "/wallets" => {
                        let count = wt.scores.len();
                        let mut wallet_list = format!("👛 Watching {} wallets:\n", count);
                        for entry in wt.scores.iter().take(10) {
                            let addr = format!("{:?}", entry.key());
                            let short = format!("0x{}…{}", &addr[2..8], &addr[38..42]);
                            let score = entry.value();
                            wallet_list.push_str(&format!("  {} — {:.1}% WR, {} trades\n", 
                                short, score.win_rate, score.trade_count));
                        }
                        let _ = m_bot.send_message(msg.chat.id, wallet_list).await;
                    },
                    "/addwallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                wt.add_wallet(addr);
                                let _ = m_bot.send_message(msg.chat.id, format!("✅ Added {}", parts[1])).await;
                            } else {
                                let _ = m_bot.send_message(msg.chat.id, "❌ Invalid address format").await;
                            }
                        } else {
                            let _ = m_bot.send_message(msg.chat.id, "Usage: /addwallet 0x...").await;
                        }
                    },
                    "/removewallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                if wt.remove_wallet(&addr) {
                                    let _ = m_bot.send_message(msg.chat.id, format!("✅ Removed {}", parts[1])).await;
                                } else {
                                    let _ = m_bot.send_message(msg.chat.id, "❌ Wallet not found").await;
                                }
                            }
                        } else {
                            let _ = m_bot.send_message(msg.chat.id, "Usage: /removewallet 0x...").await;
                        }
                    },
                    "/risk" => {
                        let r = conf.read().await.risk.clone();
                        let msg_text = format!("🛡️ Risk Limits\nMax Daily Loss: ${}\nMax Positions: {}\nMax Slippage: {}%\nLoss Halt: {} consecutive", 
                            r.daily_max_loss_usdc, r.max_open_positions, r.max_slippage_pct, r.consecutive_loss_halt);
                        let _ = m_bot.send_message(msg.chat.id, msg_text).await;
                    },
                    "/balance" => {
                        let _ = m_bot.send_message(msg.chat.id, "💰 Balance: See TUI dashboard for live balance.").await;
                    },
                    "/trades" => {
                        let _ = m_bot.send_message(msg.chat.id, "📊 Recent trades: See TUI Live Trade Feed.").await;
                    },
                    "/pnl" => {
                        let _ = m_bot.send_message(msg.chat.id, "📈 P&L: See TUI P&L bar for session stats.").await;
                    },
                    _ => {
                        let _ = m_bot.send_message(msg.chat.id, "❓ Unknown command. Type /help for available commands.").await;
                    }
                }
            }
            Ok(())
        }
    })
    .await;
}
