use sqlx::{Pool, Postgres};
use anyhow::Result;
use tracing::info;
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
}
