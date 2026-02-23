# POLY-APEX 🧠

High-frequency Rust copy trading bot for Polymarket. WebSocket ingestion, Kelly sizing, circuit breakers, Telegram control.

## Architecture

```
RTDS WebSocket → CopyEngine (Kelly) → RiskGate → CLOB Executor → Settlement
                                                        ↕
                                              TimescaleDB Flush (30s)
```

## Setup

1. Fill `.env` from `.env.example`
2. Edit `config.toml` for wallets, risk limits, sizing mode
3. `cargo run --release`

## Telegram Commands

| Command | Action |
|---------|--------|
| `/status` | Portfolio + open positions |
| `/pause` / `/resume` | Toggle copy engine |
| `/wallets` | List tracked wallets |
| `/addwallet 0x...` | Add target wallet |
| `/removewallet 0x...` | Remove target wallet |
| `/risk` | Current risk parameters |
| `/balance` | USDC balance |
| `/trades` | Recent trade log |
| `/pnl` | P&L summary |

## Key Config (`config.toml`)

```toml
[copy]
sizing_mode = "kelly"
min_market_liquidity_usdc = 100.0
preview_mode = true  # dry-run, no real orders

[risk]
max_slippage_pct = 2.5
consecutive_loss_halt = 3
max_open_positions = 5

[wallets]
auto_discover = true
max_watched_wallets = 20
```
