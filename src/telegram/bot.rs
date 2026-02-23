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
    _wallet_tracker: Arc<WalletTracker>,
    _position_tracker: Arc<PositionTracker>,
) {
    let token = config.read().await.telegram.bot_token.clone();
    if token.is_empty() {
        warn!("Telegram bot token empty, skipping bot startup");
        return;
    }

    let bot = Bot::new(token);
    
    info!("Starting Telegram bot...");
    
    let cloned_state = bot_state.clone();
    teloxide::repl(bot, move |m_bot: Bot, msg: Message| {
        let state = cloned_state.clone();
        async move {
            if let Some(text) = msg.text() {
                if text == "/pause" {
                    *state.write().await = BotState::Paused;
                    let _ = m_bot.send_message(msg.chat.id, "Bot Paused").await;
                } else if text == "/resume" {
                    *state.write().await = BotState::Running;
                    let _ = m_bot.send_message(msg.chat.id, "Bot Resumed").await;
                } else if text == "/status" {
                    let s = *state.read().await;
                    let _ = m_bot.send_message(msg.chat.id, format!("Bot State: {:?}", s)).await;
                }
            }
            Ok(())
        }
    })
    .await;
}
