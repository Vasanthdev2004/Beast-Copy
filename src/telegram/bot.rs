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
                        let _ = m_bot.send_message(msg.chat.id, format!("Watching {} wallets.", count)).await;
                    },
                    "/addwallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                wt.add_wallet(addr);
                                let _ = m_bot.send_message(msg.chat.id, format!("Added {}", parts[1])).await;
                            } else {
                                let _ = m_bot.send_message(msg.chat.id, "Invalid address format").await;
                            }
                        }
                    },
                    "/removewallet" => {
                        if parts.len() > 1 {
                            if let Ok(addr) = parts[1].parse::<alloy_primitives::Address>() {
                                if wt.remove_wallet(&addr) {
                                    let _ = m_bot.send_message(msg.chat.id, format!("Removed {}", parts[1])).await;
                                } else {
                                    let _ = m_bot.send_message(msg.chat.id, "Wallet not found").await;
                                }
                            }
                        }
                    },
                    "/risk" => {
                        let r = conf.read().await.risk.clone();
                        let msg_text = format!("🛡️ Risk Limits\nMax Daily Loss: ${}\nMax Slippage: {}%", r.daily_max_loss_usdc, r.max_slippage_pct);
                        let _ = m_bot.send_message(msg.chat.id, msg_text).await;
                    },
                    "/balance" => {
                        let _ = m_bot.send_message(msg.chat.id, "Balance fetch initialized. See logs.").await;
                    },
                    "/trades" => {
                        let _ = m_bot.send_message(msg.chat.id, "Recent trades: [Feature Pending]").await;
                    },
                    "/pnl" => {
                        let _ = m_bot.send_message(msg.chat.id, "7-day PnL: [Feature Pending]").await;
                    },
                    _ => {
                        let _ = m_bot.send_message(msg.chat.id, "Unknown Command.").await;
                    }
                }
            }
            Ok(())
        }
    })
    .await;
}
