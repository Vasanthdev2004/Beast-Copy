use sqlx::{Pool, Postgres};
use anyhow::Result;
use tracing::info;
use crate::types::{Position, WalletScore};

pub struct DbClient {
    _pool: Pool<Postgres>,
}

impl DbClient {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
            
        Ok(Self { _pool: pool })
    }

    pub async fn flush_position(&self, position: &Position) -> Result<()> {
        info!("Flushing position to TSDB: {:?}", position.market_id);
        // INSERT INTO trades (...) VALUES (...)
        Ok(())
    }
    
    pub async fn flush_wallet_score(&self, score: &WalletScore) -> Result<()> {
        info!("Flushing wallet score to TSDB: {:?}", score.address);
        // UPDATE wallet_scores SET ...
        Ok(())
    }
}
