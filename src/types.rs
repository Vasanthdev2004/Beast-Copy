use alloy_primitives::{Address, B256 as H256};
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Side {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    FOK,
    GTC,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum BotState {
    Running,
    Paused,
    Halted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum OrderStatus {
    Filled,
    Rejected,
    Timeout,
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    pub asset_id: String,
    pub wallet: Address,
    pub side: Side,
    pub size: Decimal,
    pub price: Decimal,
    pub market_id: String,
    pub market_name: String,
    pub timestamp_ms: u64,
    pub is_sell: bool,
}

#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub market_id: String,
    pub market_name: String,
    pub asset_id: String,
    pub side: Side,
    pub size: Decimal,
    pub price: Decimal,
    pub order_type: OrderType,
    pub source_wallet: Address,
    pub is_sell: bool,
}

#[derive(Debug, Clone)]
pub struct OrderResult {
    pub order_id: String,
    pub status: OrderStatus,
    pub tx_hash: Option<H256>,
    pub filled_at: Decimal,
    pub timestamp: u64,
    pub market_id: String,
    pub market_name: String,
    pub asset_id: String,
    pub side: Side,
    pub size: Decimal,
    pub source_wallet: Address,
    pub is_paper: bool,
    pub is_sell: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Position {
    pub market_id: String,
    pub market_name: String,
    pub side: Side,
    pub size: Decimal,
    pub entry_price: Decimal,
    pub source_wallet: Address,
    pub opened_at: u64,
    pub pnl: Option<Decimal>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WalletScore {
    pub address: Address,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub trade_count: u32,
    pub last_active: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub time: String,
    pub kind: String,
    pub message: String,
}
