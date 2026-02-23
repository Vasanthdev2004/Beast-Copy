use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::io;
use tokio::sync::RwLock;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use ratatui::widgets::*;
use ratatui::style::{Color, Modifier, Style};
use rust_decimal::Decimal;

use crate::config::AppConfig;
use crate::engines::position_tracker::PositionTracker;
use crate::engines::wallet_tracker::WalletTracker;
use crate::types::BotState;

/// Scrollable trade log entry
#[derive(Clone)]
pub struct LogEntry {
    pub time: String,
    pub kind: String,  // COPY, SKIP, FILL, RISK, etc.
    pub message: String,
}

/// Shared dashboard state fed from other modules
pub struct DashboardState {
    pub config: Arc<RwLock<AppConfig>>,
    pub bot_state: Arc<RwLock<BotState>>,
    pub wallet_tracker: Arc<WalletTracker>,
    pub position_tracker: Arc<PositionTracker>,
    pub consecutive_losses: Arc<AtomicUsize>,
    pub trade_log: Arc<RwLock<Vec<LogEntry>>>,
    pub usdc_balance: Arc<RwLock<Decimal>>,
}

impl DashboardState {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        bot_state: Arc<RwLock<BotState>>,
        wallet_tracker: Arc<WalletTracker>,
        position_tracker: Arc<PositionTracker>,
        consecutive_losses: Arc<AtomicUsize>,
        usdc_balance: Arc<RwLock<Decimal>>,
    ) -> Self {
        Self {
            config,
            bot_state,
            wallet_tracker,
            position_tracker,
            consecutive_losses,
            trade_log: Arc::new(RwLock::new(Vec::new())),
            usdc_balance,
        }
    }
}

/// Push a log entry to the dashboard feed (call from any module)
pub async fn push_log(state: &DashboardState, kind: &str, message: &str) {
    let entry = LogEntry {
        time: chrono::Utc::now().format("%H:%M:%S").to_string(),
        kind: kind.to_string(),
        message: message.to_string(),
    };
    let mut log = state.trade_log.write().await;
    log.push(entry);
    // Keep last 200 entries
    if log.len() > 200 {
        let drain_to = log.len() - 200;
        log.drain(0..drain_to);
    }
}

/// Run the TUI dashboard (blocks on the current task, handles input)
pub async fn run_dashboard(state: Arc<DashboardState>) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    loop {
        // Gather snapshot data
        let bot_st = *state.bot_state.read().await;
        let config = state.config.read().await.clone();
        let balance = *state.usdc_balance.read().await;
        let losses = state.consecutive_losses.load(Ordering::Relaxed);
        let log_entries = state.trade_log.read().await.clone();

        // Positions
        let positions: Vec<_> = state.position_tracker.positions.iter()
            .map(|e| e.value().clone())
            .collect();
        let open_count = positions.len();
        let max_positions = config.risk.max_open_positions;

        // Wallets
        let wallets: Vec<_> = state.wallet_tracker.scores.iter()
            .map(|e| e.value().clone())
            .collect();

        let preview = config.copy.preview_mode;
        let loss_halt = config.risk.consecutive_loss_halt;

        terminal.draw(|frame| {
            let area = frame.area();

            // Main layout: header + body
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),   // Header
                    Constraint::Min(10),     // Body
                    Constraint::Length(3),    // Footer / Risk bar
                ])
                .split(area);

            // ── Header ──
            let mode_str = if preview { "PAPER" } else { "LIVE" };
            let mode_color = if preview { Color::Yellow } else { Color::Red };
            let state_str = match bot_st {
                BotState::Running => "RUNNING",
                BotState::Paused => "PAUSED",
                BotState::Halted => "HALTED",
            };
            let state_color = match bot_st {
                BotState::Running => Color::Green,
                BotState::Paused => Color::Yellow,
                BotState::Halted => Color::Red,
            };

            let balance_str = format!(" │ Balance: ${} USDC ", balance);
            let positions_str = format!("│ Positions: {}/{} ", open_count, max_positions);

            let header_text = Line::from(vec![
                Span::styled(" ◆ POLY-APEX ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("│ "),
                Span::styled(state_str, Style::default().fg(state_color).add_modifier(Modifier::BOLD)),
                Span::raw(" │ Mode: "),
                Span::styled(mode_str, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
                Span::raw(&balance_str),
                Span::raw(&positions_str),
            ]);
            let header = Paragraph::new(header_text)
                .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            frame.render_widget(header, main_chunks[0]);

            // ── Body: left (positions + wallets) | right (trade feed) ──
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(55),
                    Constraint::Percentage(45),
                ])
                .split(main_chunks[1]);

            // Left: Positions on top, Wallets on bottom
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(55),
                    Constraint::Percentage(45),
                ])
                .split(body_chunks[0]);

            // ── Positions Table ──
            let pos_header = Row::new(vec!["Market", "Side", "Size", "Entry", "P&L"])
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .bottom_margin(1);

            let pos_rows: Vec<Row> = positions.iter().map(|p| {
                let side_str = match p.side {
                    crate::types::Side::Yes => "YES",
                    crate::types::Side::No => "NO",
                };
                let pnl_str = p.pnl.map(|v| format!("{}", v)).unwrap_or_else(|| "—".to_string());
                let pnl_color = p.pnl.map(|v| {
                    if v > Decimal::ZERO { Color::Green } else { Color::Red }
                }).unwrap_or(Color::DarkGray);

                let market_short = if p.market_id.len() > 20 {
                    format!("{}…", &p.market_id[..20])
                } else {
                    p.market_id.clone()
                };

                Row::new(vec![
                    Cell::from(market_short),
                    Cell::from(side_str).style(Style::default().fg(
                        if side_str == "YES" { Color::Green } else { Color::Red }
                    )),
                    Cell::from(format!("${}", p.size)),
                    Cell::from(format!("{}", p.entry_price)),
                    Cell::from(pnl_str).style(Style::default().fg(pnl_color)),
                ])
            }).collect();

            let pos_table = Table::new(
                pos_rows,
                [
                    Constraint::Percentage(35),
                    Constraint::Percentage(10),
                    Constraint::Percentage(18),
                    Constraint::Percentage(18),
                    Constraint::Percentage(19),
                ],
            )
            .header(pos_header)
            .block(Block::default()
                .title(format!(" Open Positions ({}/{}) ", open_count, max_positions))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)));
            frame.render_widget(pos_table, left_chunks[0]);

            // ── Wallets Table ──
            let wal_header = Row::new(vec!["Address", "Win%", "Trades", "PnL"])
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .bottom_margin(1);

            let wal_rows: Vec<Row> = wallets.iter().take(10).map(|w| {
                let addr_short = format!("0x{}…{}", 
                    &format!("{:?}", w.address)[2..6],
                    &format!("{:?}", w.address)[38..42],
                );
                let wr_color = if w.win_rate >= 55.0 { Color::Green } else { Color::Yellow };
                Row::new(vec![
                    Cell::from(addr_short),
                    Cell::from(format!("{:.1}%", w.win_rate)).style(Style::default().fg(wr_color)),
                    Cell::from(format!("{}", w.trade_count)),
                    Cell::from(format!("{}", w.total_pnl)),
                ])
            }).collect();

            let wal_table = Table::new(
                wal_rows,
                [
                    Constraint::Percentage(35),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(25),
                ],
            )
            .header(wal_header)
            .block(Block::default()
                .title(format!(" Tracked Wallets ({}) ", wallets.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta)));
            frame.render_widget(wal_table, left_chunks[1]);

            // ── Trade Feed (right panel) ──
            let feed_items: Vec<ListItem> = log_entries.iter().rev().take(50).map(|entry| {
                let kind_color = match entry.kind.as_str() {
                    "COPY" => Color::Green,
                    "FILL" => Color::Cyan,
                    "SKIP" => Color::DarkGray,
                    "RISK" => Color::Yellow,
                    "ERR" => Color::Red,
                    _ => Color::White,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(&entry.time, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<5}", entry.kind),
                        Style::default().fg(kind_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::raw(&entry.message),
                ]))
            }).collect();

            let feed = List::new(feed_items)
                .block(Block::default()
                    .title(" Live Trade Feed ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)));
            frame.render_widget(feed, body_chunks[1]);

            // ── Footer / Risk Bar ──
            let loss_pct = if loss_halt > 0 {
                (losses as f64 / loss_halt as f64 * 100.0).min(100.0) as u16
            } else { 0 };

            let risk_gauge = Gauge::default()
                .block(Block::default()
                    .title(format!(" Risk │ Losses: {}/{} │ {} ",
                        losses, loss_halt,
                        if losses >= loss_halt { "⚠ HALTED" } else { "OK" }
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(
                        if losses >= loss_halt { Color::Red } else { Color::DarkGray }
                    )))
                .gauge_style(Style::default().fg(
                    if loss_pct > 66 { Color::Red } else if loss_pct > 33 { Color::Yellow } else { Color::Green }
                ))
                .ratio(loss_pct as f64 / 100.0)
                .label(format!("{}%", loss_pct));
            frame.render_widget(risk_gauge, main_chunks[2]);
        })?;

        // Handle input (non-blocking, 250ms tick)
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('p') => {
                            let mut st = state.bot_state.write().await;
                            *st = match *st {
                                BotState::Running => BotState::Paused,
                                BotState::Paused => BotState::Running,
                                BotState::Halted => BotState::Halted,
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
