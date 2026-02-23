# POLY-APEX 🧠⚡

> High-frequency, event-driven Rust copy trading bot for [Polymarket](https://polymarket.com). Sub-30ms latency from signal to order.

---

## How It Works

POLY-APEX connects to Polymarket's real-time WebSocket feed (RTDS), watches the on-chain trades of top-performing wallets, and mirrors their positions using Kelly Criterion sizing. Every trade passes through a multi-layer risk gate before hitting the CLOB order book.

```
                         ┌─────────────────────┐
                         │  Polymarket RTDS WS  │
                         │  (Live Trade Feed)   │
                         └──────────┬───────────┘
                                    │ TradeEvent
                                    ▼
┌──────────────┐        ┌───────────────────────┐
│  Wallet      │◄──────►│     Copy Engine        │
│  Tracker     │ scores │  • Dedup filter        │
│  (DashMap)   │        │  • Kelly sizing        │
│  + Gamma API │        │  • USDC balance fetch  │
└──────────────┘        └──────────┬────────────┘
                                   │ OrderIntent
                                   ▼
                        ┌───────────────────────┐
                        │      Risk Gate         │
                        │  • Max open positions  │
                        │  • Consecutive losses  │
                        │  • Slippage bounds     │
                        │  • Circuit breaker     │
                        └──────────┬────────────┘
                                   │ OrderIntent (approved)
                                   ▼
                        ┌───────────────────────┐
                        │    CLOB Executor       │
                        │  • polymarket-rs SDK   │
                        │  • EIP-712 signing     │
                        │  • Preview mode        │
                        └──────────┬────────────┘
                                   │ OrderResult
                                   ▼
                        ┌───────────────────────┐
                        │  Settlement Monitor    │
                        │  • Confirms fills      │
                        │  • Tracks wins/losses  │
                        │  • Updates positions   │
                        └──────────┬────────────┘
                                   │
                          ┌────────┴────────┐
                          ▼                 ▼
                   ┌────────────┐   ┌──────────────┐
                   │  DashMap   │   │ TimescaleDB  │
                   │ (Hot Path) │   │ (30s flush)  │
                   └────────────┘   └──────────────┘
```

---

## Project Structure

```
src/
├── main.rs                    # Startup, channel wiring, task spawning
├── config.rs                  # TOML config + hot-reload watcher
├── types.rs                   # Shared types (TradeEvent, OrderIntent, Position, etc.)
│
├── engines/
│   ├── rtds_engine.rs         # WebSocket connection to Polymarket RTDS
│   ├── copy_engine.rs         # Core copy logic + Kelly sizing + USDC balance
│   ├── wallet_tracker.rs      # Wallet scoring + Gamma API auto-discovery
│   └── position_tracker.rs    # Open position tracking (DashMap)
│
├── execution/
│   ├── clob_executor.rs       # Order submission via polymarket-rs TradingClient
│   └── settlement.rs          # Fill confirmation + win/loss tracking
│
├── risk/
│   └── risk_gate.rs           # Circuit breaker, max positions, slippage checks
│
├── storage/
│   └── db.rs                  # TimescaleDB async flush (trades + wallet_scores)
│
├── telegram/
│   └── bot.rs                 # Telegram control surface (auth + 10 commands)
│
└── utils/
    └── logger.rs              # Tracing subscriber init
```

---

## Prerequisites

- **Rust** 1.75+ (`rustup update stable`)
- **PostgreSQL** 14+ with TimescaleDB extension (optional — bot runs without it)
- **Polymarket API credentials** (API key, secret, passphrase)
- **Polygon wallet** with USDC + private key for signing
- **Telegram bot token** from [@BotFather](https://t.me/BotFather)

---

## Setup

### 1. Clone & Configure

```bash
git clone <your-repo-url>
cd Beast-Copy
cp .env.example .env
```

Edit `.env`:
```env
# Polymarket CLOB API
POLYMARKET_API_KEY=your_api_key
POLYMARKET_API_SECRET=your_api_secret
POLYMARKET_API_PASSPHRASE=your_passphrase

# Wallet (Polygon)
WALLET_PRIVATE_KEY=0xYOUR_PRIVATE_KEY
WALLET_PUBLIC_ADDRESS=0xYOUR_PUBLIC_ADDRESS
POLYGON_RPC=https://polygon-mainnet.g.alchemy.com/v2/YOUR_KEY

# Telegram
TELEGRAM_BOT_TOKEN=your_bot_token

# Database (optional)
DATABASE_URL=postgres://user:pass@localhost:5432/poly_apex
```

### 2. Edit `config.toml`

```toml
[wallets]
targets = [
    "0xWALLET_1_TO_COPY",
    "0xWALLET_2_TO_COPY",
]
auto_discover = true           # Pull top wallets from Gamma API every 6h
max_watched_wallets = 20

[copy]
preview_mode = true            # DRY RUN — no real orders. Set false to go live.
sizing_mode = "kelly"          # "kelly" | "fixed" | "ratio"
copy_ratio = 0.10              # Used if sizing_mode = "ratio"
fixed_usdc = 5.0               # Used if sizing_mode = "fixed"
max_single_trade_usdc = 50.0
min_market_liquidity_usdc = 100.0
min_copy_size_usdc = 1.0
cooldown_per_market_secs = 300
dedup_window_secs = 60

[risk]
daily_max_loss_usdc = 100.0
max_open_positions = 5
max_slippage_pct = 2.5
consecutive_loss_halt = 3      # Pause after 3 consecutive losses
halt_duration_mins = 30

[rpc]
polygon_rpc = "https://polygon-mainnet.g.alchemy.com/v2/YOUR_KEY"
backup_rpc = "https://polygon-rpc.com"
confirmation_timeout_secs = 30

[telegram]
bot_token = ""                 # Overridden by .env
allowed_user_ids = [123456789] # Your Telegram user ID
```

### 3. Run

```bash
# Preview mode (dry run, no real money)
cargo run --release

# Or use the batch script
./run.bat
```

### 4. Database Setup (Optional)

If you want persistent trade history:

```sql
CREATE TABLE trades (
    id SERIAL PRIMARY KEY,
    market_id TEXT NOT NULL,
    side TEXT NOT NULL,
    size DECIMAL NOT NULL,
    entry_price DECIMAL NOT NULL,
    source_wallet TEXT NOT NULL,
    opened_at BIGINT NOT NULL,
    pnl DECIMAL DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE wallet_scores (
    address TEXT PRIMARY KEY,
    win_rate DOUBLE PRECISION NOT NULL,
    total_pnl DECIMAL NOT NULL,
    trade_count INTEGER NOT NULL,
    last_active BIGINT NOT NULL,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);
```

The bot flushes in-memory state to these tables every 30 seconds. If the database is unavailable, the bot continues running with in-memory storage only.

---

## Telegram Commands

All commands require your Telegram user ID to be in `allowed_user_ids`. Unauthorized users are silently ignored.

| Command | Description |
|---|---|
| `/status` | Bot state, open positions count, portfolio value |
| `/pause` | Pause the copy engine (stops new trades) |
| `/resume` | Resume the copy engine |
| `/wallets` | List all tracked wallets with scores |
| `/addwallet 0x...` | Start tracking a new wallet |
| `/removewallet 0x...` | Stop tracking a wallet |
| `/risk` | Show current risk gate parameters |
| `/balance` | Current USDC balance on Polygon |
| `/trades` | Recent trade log |
| `/pnl` | Profit & loss summary |

---

## Sizing Modes

| Mode | Behavior |
|---|---|
| `kelly` | Dynamic sizing based on wallet win rate and market odds. Higher edge → bigger position. |
| `fixed` | Every trade uses `fixed_usdc` amount. Simple and predictable. |
| `ratio` | Mirrors `copy_ratio` of the source wallet's trade size. |

---

## Risk Controls

| Control | Config Key | Description |
|---|---|---|
| **Max Open Positions** | `max_open_positions` | Hard cap on concurrent trades |
| **Consecutive Loss Halt** | `consecutive_loss_halt` | Auto-pause after N losses in a row |
| **Daily Max Loss** | `daily_max_loss_usdc` | Stop trading if daily loss exceeds limit |
| **Slippage Guard** | `max_slippage_pct` | Reject orders on extreme-priced markets (<1¢ or >99¢) |
| **Cooldown** | `cooldown_per_market_secs` | Wait time before re-entering the same market |
| **Dedup Window** | `dedup_window_secs` | Ignore duplicate trade signals within window |

---

## Key Design Decisions

- **DashMap over Mutex** — Lock-free concurrent reads for position tracking. No contention between the copy engine and risk gate.
- **Tokio broadcast channels** — RTDS events fan out to multiple consumers without cloning overhead.
- **Hot-reloadable config** — Edit `config.toml` while running. Changes take effect within 100ms via `notify` file watcher.
- **Graceful DB degradation** — If TimescaleDB is down, bot runs fully in-memory. No crash, just a warning log.
- **Preview mode first** — Always test with `preview_mode = true` before going live. Preview generates realistic `OrderResult` events so you can validate the full pipeline end-to-end.

---

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `POLYMARKET_API_KEY` | Yes (live mode) | CLOB API key |
| `POLYMARKET_API_SECRET` | Yes (live mode) | CLOB API secret |
| `POLYMARKET_API_PASSPHRASE` | Yes (live mode) | CLOB API passphrase |
| `WALLET_PRIVATE_KEY` | Yes (live mode) | Polygon wallet private key for EIP-712 signing |
| `WALLET_PUBLIC_ADDRESS` | Yes | Public address for USDC balance checks |
| `POLYGON_RPC` | Yes | Alchemy/Infura Polygon RPC URL |
| `TELEGRAM_BOT_TOKEN` | Yes | Telegram bot token |
| `DATABASE_URL` | No | PostgreSQL connection string |

---

## Dependencies

| Crate | Purpose |
|---|---|
| `polymarket-rs` | Polymarket CLOB/Gamma SDK (order signing, posting, cancellation) |
| `alloy-signer-local` | EIP-712 wallet signing (matched to polymarket-rs v0.7.3) |
| `alloy-primitives` | Ethereum Address/B256 types |
| `tokio` | Async runtime |
| `tokio-tungstenite` | WebSocket client for RTDS |
| `dashmap` | Lock-free concurrent hashmap |
| `sqlx` | Async PostgreSQL driver |
| `teloxide` | Telegram bot framework |
| `rust_decimal` | Precise decimal math for prices/sizes |
| `reqwest` | HTTP client for Gamma API + balance RPC |
| `notify` | File system watcher for config hot-reload |
| `tracing` | Structured logging |

---

## License

Private — not for redistribution.
