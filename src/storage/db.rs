use sqlx::{Pool, Postgres};
use anyhow::Result;
use tracing::info;
use sqlx::Row;
use crate::types::{Position, WalletScore};

pub struct DbClient {
    pool: Pool<Postgres>,
}

impl DbClient {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
            
        Ok(Self { pool })
    }

    pub async fn flush_position(&self, position: &Position) -> Result<()> {
        info!("Flushing position to TSDB: {:?}", position.market_id);
        let side_str = match position.side {
            crate::types::Side::Yes => "Yes",
            crate::types::Side::No => "No",
        };
        
        let pnl = position.pnl.unwrap_or(rust_decimal_macros::dec!(0.0));
        let addr = position.source_wallet.to_string();

        sqlx::query(
            r#"
            INSERT INTO trades (market_id, side, size, entry_price, source_wallet, opened_at, pnl)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (market_id, side, source_wallet) DO UPDATE SET
                size = EXCLUDED.size,
                pnl = EXCLUDED.pnl
            "#,
        )
        .bind(&position.market_id)
        .bind(side_str)
        .bind(&position.size)
        .bind(&position.entry_price)
        .bind(&addr)
        .bind(position.opened_at as i64)
        .bind(pnl)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
    
    pub async fn flush_wallet_score(&self, score: &WalletScore) -> Result<()> {
        info!("Flushing wallet score to TSDB: {:?}", score.address);
        let addr = score.address.to_string();
        sqlx::query(
            r#"
            INSERT INTO wallet_scores (address, win_rate, total_pnl, trade_count, last_active)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (address) DO UPDATE SET
                win_rate = EXCLUDED.win_rate,
                total_pnl = EXCLUDED.total_pnl,
                trade_count = EXCLUDED.trade_count,
                last_active = EXCLUDED.last_active
            "#,
        )
        .bind(&addr)
        .bind(score.win_rate)
        .bind(score.total_pnl)
        .bind(score.trade_count as i32)
        .bind(score.last_active as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn load_active_positions(&self) -> Result<Vec<Position>> {
        info!("Loading active positions from TSDB on startup...");
        let rows = sqlx::query(
            r#"
            SELECT market_id, side, size, entry_price, source_wallet, opened_at, pnl 
            FROM trades
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut positions = Vec::new();
        for row in rows {
            let side_str: String = row.get("side");
            let side = if side_str == "Yes" {
                crate::types::Side::Yes
            } else {
                crate::types::Side::No
            };

            let sw_str: String = row.get("source_wallet");
            let source_wallet = match std::str::FromStr::from_str(&sw_str) {
                Ok(addr) => addr,
                Err(_) => continue,
            };

            let opened_at_i64: i64 = row.get("opened_at");
            
            positions.push(Position {
                market_id: row.get("market_id"),
                market_name: String::new(),
                side,
                size: row.get("size"),
                entry_price: row.get("entry_price"),
                source_wallet,
                opened_at: opened_at_i64 as u64,
                pnl: Some(row.get("pnl")),
            });
        }
        
        info!("Loaded {} active positions from DB.", positions.len());
        Ok(positions)
    }

    pub async fn delete_position(&self, market_id: &str, side_str: &str, wallet: &str) -> Result<()> {
        sqlx::query(
            r#"
            DELETE FROM trades WHERE market_id = $1 AND side = $2 AND source_wallet = $3
            "#
        )
        .bind(market_id)
        .bind(side_str)
        .bind(wallet)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
