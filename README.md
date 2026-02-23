# POLY-APEX 🧠

**POLY-APEX** is a completely rebuilt, high-frequency, event-driven Rust copy trading bot for Polymarket. It replaces legacy polling architectures with real-time WebSocket ingestion, in-memory Dashmap state tracking, and asynchronous TimescaleDB database flushing, capable of executing sub-30ms latency copy trades.

## 🚀 Features
*   **Real-time WEBSOCKET Ingestion (RTDS)**: Connects directly to Polymarket's `ws-subscriptions-clob` rather than polling the Data API.
*   **Dynamic Sizing (Kelly Criterion)**: Replaces static sizing logic with dynamic Kelly Criterion sizing based on wallet win rates and odds probabilities.
*   **Auto-Discovery & Gamma Leadboard Tracker**: Continuously pulls top active wallets from Polymarket API and ranks them by 30-day ROI. 
*   **Full Circuit Breakers & Risk Control**: Hardware-level risk gating on maximum slippage, maximum open positions, and configurable consecutive-loss halters.
*   **Zero-Latency Storage Engine**: `DashMap` hot-path engine enables multi-threaded concurrent execution without database round-trip times, flushing to TimescaleDB asynchronously over `sqlx`.
*   **Live Telegram Control Surface**: Start, pause, view positions, and adjust configurations live via an integrated Teloxide responder module.

---

## 🏗️ Architecture

```text
┌─────────────────────────────────────────────────────────┐
│                    POLY-APEX CORE                        │
│                                                         │
│  ┌──────────────┐    ┌─────────────────────────────┐   │
│  │  RTDS Engine │───▶│    Event Bus (Tokio MPSC)   │   │
│  │  WebSocket   │    │                             │   │
│  └──────────────┘    └──────────┬──────────────────┘   │
│                                 │                        │
│          ┌──────────────────────┼──────────────────┐    │
│          ▼                      ▼                   ▼    │
│  ┌──────────────┐   ┌─────────────────┐  ┌──────────────┐│
│  │ Copy Engine  │   │  Arb Engine     │  │  MM Engine  ││
│  │  (Primary)   │   │  (Secondary)    │  │  (Passive)  ││
│  └──────┬───────┘   └────────┬────────┘  └──────┬───────┘│
│         │                    │                   │        │
│         └──────────┬─────────┘                   │        │
│                    ▼                              │        │
│         ┌─────────────────────┐                  │        │
│         │   Risk Gate         │◀─────────────────┘        │
│         │  (Circuit Breaker)  │                           │
│         └──────────┬──────────┘                          │
│                    ▼                                      │
│         ┌─────────────────────┐                          │
│         │   CLOB Executor     │                          │
│         │  (rs-clob-client)   │                          │
│         └──────────┬──────────┘                          │
│                    ▼                                      │
│         ┌─────────────────────┐                          │
│         │  Settlement Monitor │                          │
│         │  (Polygon RPC)      │                          │
│         └──────────┬──────────┘                          │
│                    ▼                                      │
│         ┌─────────────────────┐                          │
│         │  Storage Layer      │                          │
│         │  DashMap + TSDB     │                          │
│         └─────────────────────┘                          │
└─────────────────────────────────────────────────────────┘
```

## ⚙️ Quick Start 
**1. Configure the environment:**
Create a `.env` file referencing `.env.example`.
```env
POLYMARKET_API_KEY=...
WALLET_PRIVATE_KEY=...
TELEGRAM_BOT_TOKEN=...
```

**2. Configure the behavior (`config.toml`):**
Set the baseline copy logic and max limits.
```toml
[copy]
sizing_mode = "kelly"
min_market_liquidity_usdc = 100.0

[risk]
max_slippage_pct = 2.5
consecutive_loss_halt = 3
```

**3. Run:**
You can utilize the predefined `.bat` script, or run:
```bash
cargo run --release
```

## 📱 Telegram Admin Commands
Connect directly to the bot inside Telegram to utilize:
* `/status` — View portfolio and open positions.
* `/pause` / `/resume` — Halt copying logic remotely.
* `/wallets` — Inspect tracked wallets.
* `/addwallet 0x...` / `/removewallet 0x...` — Adjust targets on the fly.
* `/risk` — Output the currently active Risk Gate parameters.
