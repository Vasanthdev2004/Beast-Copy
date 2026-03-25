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
    pub kind: String,
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
    pub initial_balance: Decimal,
    pub start_time: std::time::Instant,
}

impl DashboardState {
    pub fn new(
        config: Arc<RwLock<AppConfig>>,
        bot_state: Arc<RwLock<BotState>>,
        wallet_tracker: Arc<WalletTracker>,
        position_tracker: Arc<PositionTracker>,
        consecutive_losses: Arc<AtomicUsize>,
        usdc_balance: Arc<RwLock<Decimal>>,
        initial_balance: Decimal,
    ) -> Self {
        Self {
            config,
            bot_state,
            wallet_tracker,
            position_tracker,
            consecutive_losses,
            trade_log: Arc::new(RwLock::new(Vec::new())),
            usdc_balance,
            initial_balance,
            start_time: std::time::Instant::now(),
        }
    }
}

/// Run the TUI dashboard (blocks on the current task, handles input)
pub async fn run_dashboard(
    state: Arc<DashboardState>,
    mut log_rx: tokio::sync::mpsc::UnboundedReceiver<LogEntry>,
) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    loop {
        // Drain incoming logs
        {
            let mut log = state.trade_log.write().await;
            while let Ok(entry) = log_rx.try_recv() {
                log.push(entry);
            }
            if log.len() > 500 {
                let drain_to = log.len() - 500;
                log.drain(0..drain_to);
            }
        }

        // Gather snapshot data
        let bot_st = *state.bot_state.read().await;
        let config = state.config.read().await.clone();
        let balance = *state.usdc_balance.read().await;
        let losses = state.consecutive_losses.load(Ordering::Relaxed);
        let log_entries = state.trade_log.read().await.clone();

        let positions: Vec<_> = state.position_tracker.positions.iter()
            .map(|e| e.value().clone())
            .collect();
        let open_count = positions.len();
        let _max_positions = config.risk.max_open_positions;

        let wallets: Vec<_> = state.wallet_tracker.scores.iter()
            .map(|e| e.value().clone())
            .collect();

        let preview = config.copy.preview_mode;
        let loss_halt = config.risk.consecutive_loss_halt;

        // Compute stats
        let session_pnl = balance - state.initial_balance;
        let elapsed = state.start_time.elapsed();
        let uptime_str = format!("{}m {}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60);

        // Count trade types from log
        let copy_count = log_entries.iter().filter(|e| e.kind == "COPY").count();
        let fill_count = log_entries.iter().filter(|e| e.kind == "FILL").count();
        let skip_count = log_entries.iter().filter(|e| e.kind == "SKIP" || e.kind == "RISK").count();
        let hist_count = log_entries.iter().filter(|e| e.kind == "HIST").count();

        terminal.draw(|frame| {
            let area = frame.area();

            // Main layout: header + pnl bar + body + footer
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),   // Header
                    Constraint::Length(3),   // P&L Bar
                    Constraint::Min(10),     // Body
                    Constraint::Length(3),   // Footer
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

            let header_text = Line::from(vec![
                Span::styled(" ◆ POLYMARKET COPY TRADER ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("│ "),
                Span::styled(mode_str, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
                Span::raw(" │ "),
                Span::styled(state_str, Style::default().fg(state_color).add_modifier(Modifier::BOLD)),
                Span::raw(" │ ⏱ "),
                Span::styled(&uptime_str, Style::default().fg(Color::White)),
            ]);
            let header = Paragraph::new(header_text)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(Color::DarkGray)));
            frame.render_widget(header, main_chunks[0]);

            // ── P&L Status Bar ──
            let pnl_color = if session_pnl >= Decimal::ZERO { Color::Green } else { Color::Red };
            let pnl_sign = if session_pnl >= Decimal::ZERO { "+" } else { "" };

            let pnl_text = Line::from(vec![
                Span::styled(" 📈 P&L: ", Style::default().fg(Color::White)),
                Span::styled(
                    format!("{}{:.2}", pnl_sign, session_pnl.round_dp(2)),
                    Style::default().fg(pnl_color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  │  "),
                Span::styled("Portfolio: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("${:.2}", balance.round_dp(2)),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  │  "),
                Span::styled("Positions: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", open_count),
                    Style::default().fg(Color::White),
                ),
                Span::raw("  │  "),
                Span::styled("Fills: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", fill_count),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw("  │  "),
                Span::styled("Skipped: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", skip_count),
                    Style::default().fg(Color::Yellow),
                ),
            ]);
            let pnl_bar = Paragraph::new(pnl_text)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(Color::Blue)));
            frame.render_widget(pnl_bar, main_chunks[1]);

            // ── Body: left (trade feed) | right (positions + stats + whale) ──
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(55),
                    Constraint::Percentage(45),
                ])
                .split(main_chunks[2]);

            // ── Left: Live Trade Feed ──
            let feed_items: Vec<ListItem> = log_entries.iter().rev().take(100).map(|entry| {
                let kind_color = match entry.kind.as_str() {
                    "COPY" => Color::Green,
                    "FILL" => Color::Cyan,
                    "SKIP" => Color::DarkGray,
                    "RISK" => Color::Yellow,
                    "ERR" => Color::Red,
                    "HIST" => Color::Blue,
                    "SETTLE" => Color::Magenta,
                    _ => Color::White,
                };
                ListItem::new(Line::from(vec![
                    Span::styled(&entry.time, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
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
                    .title(" ⚡ LIVE TRADE FEED ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Green)));
            frame.render_widget(feed, body_chunks[0]);

            // ── Right: split into positions + statistics + whale info ──
            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(45),  // Paper Positions
                    Constraint::Length(7),       // Statistics (fixed height)
                    Constraint::Min(5),          // Target Whale (takes all remaining space)
                ])
                .split(body_chunks[1]);

            // ── Paper Positions Table ──
            let pos_header = Row::new(vec!["Market", "Side", "Size", "Entry", "P&L", "Duration"])
                .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                .bottom_margin(1);

            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            let pos_rows: Vec<Row> = positions.iter().map(|p| {
                let side_str = match p.side {
                    crate::types::Side::Yes => "YES",
                    crate::types::Side::No => "NO",
                };
                let pnl_str = p.pnl.map(|v| format!("{:+.2}", v.round_dp(2))).unwrap_or_else(|| "—".to_string());
                let pnl_color = p.pnl.map(|v| {
                    if v > Decimal::ZERO { Color::Green } else { Color::Red }
                }).unwrap_or(Color::DarkGray);

                let market_short = if p.market_id.len() > 16 {
                    format!("{}…", &p.market_id[..16])
                } else {
                    p.market_id.clone()
                };

                let duration_secs = now_ms.saturating_sub(p.opened_at) / 1000;
                let duration_str = if duration_secs < 60 {
                    format!("{}s", duration_secs)
                } else if duration_secs < 3600 {
                    format!("{}m", duration_secs / 60)
                } else {
                    format!("{}h", duration_secs / 3600)
                };

                Row::new(vec![
                    Cell::from(market_short),
                    Cell::from(side_str).style(Style::default().fg(
                        if side_str == "YES" { Color::Green } else { Color::Red }
                    )),
                    Cell::from(format!("${:.2}", p.size.round_dp(2))),
                    Cell::from(format!("¢{}", (p.entry_price * Decimal::from(100)).round_dp(0))),
                    Cell::from(pnl_str).style(Style::default().fg(pnl_color)),
                    Cell::from(duration_str).style(Style::default().fg(Color::DarkGray)),
                ])
            }).collect();

            // Total row
            let total_value: Decimal = positions.iter().map(|p| p.size).sum();

            let mut all_rows = pos_rows;
            if !positions.is_empty() {
                all_rows.push(Row::new(vec![
                    Cell::from("TOTAL").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from(""),
                    Cell::from(format!("${:.2}", total_value.round_dp(2)))
                        .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Cell::from(""),
                    Cell::from(format!("{:+.2}", session_pnl.round_dp(2)))
                        .style(Style::default().fg(pnl_color).add_modifier(Modifier::BOLD)),
                    Cell::from(""),
                ]).bottom_margin(0));
            }

            let pos_table = Table::new(
                all_rows,
                [
                    Constraint::Percentage(25),
                    Constraint::Percentage(12),
                    Constraint::Percentage(18),
                    Constraint::Percentage(15),
                    Constraint::Percentage(15),
                    Constraint::Percentage(15),
                ],
            )
            .header(pos_header)
            .block(Block::default()
                .title(" 📋 PAPER POSITIONS ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Yellow)));
            frame.render_widget(pos_table, right_chunks[0]);

            // ── Statistics Panel ──
            let whale_volume: Decimal = log_entries.iter()
                .filter(|e| e.kind == "HIST" || e.kind == "COPY")
                .filter_map(|e| {
                    // Extract dollar amounts from messages like "BUY Up $3.00 @¢28"
                    e.message.split('$').nth(1).and_then(|s| {
                        s.split_whitespace().next().and_then(|n| n.parse::<f64>().ok())
                    })
                })
                .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                .sum();

            let avg_trade = if (hist_count + copy_count) > 0 {
                whale_volume / Decimal::from(hist_count + copy_count)
            } else {
                Decimal::ZERO
            };

            let stats_text = vec![
                Line::from(vec![
                    Span::styled("  Whale Trades: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", hist_count + copy_count), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Whale Volume: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("${:.2}", whale_volume.round_dp(2)), Style::default().fg(Color::Green)),
                ]),
                Line::from(vec![
                    Span::styled("  Avg Trade:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("${:.2}", avg_trade.round_dp(2)), Style::default().fg(Color::White)),
                ]),
                Line::from(vec![
                    Span::styled("  Paper Fills:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", fill_count), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("  Skipped:      ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", skip_count), Style::default().fg(Color::Yellow)),
                ]),
            ];

            let stats = Paragraph::new(stats_text)
                .block(Block::default()
                    .title(" 📊 STATISTICS ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)));
            frame.render_widget(stats, right_chunks[1]);

            // ── Target Whale Panel ──
            let whale_lines: Vec<Line> = wallets.iter().take(50).flat_map(|w| {
                let addr_short = format!("0x{}…{}", 
                    &format!("{:?}", w.address)[2..8],
                    &format!("{:?}", w.address)[38..42],
                );
                
                let wr = w.win_rate.clamp(0.0, 100.0);
                let filled = (wr / 10.0).round() as usize;
                let empty = 10usize.saturating_sub(filled);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
                let wr_color = if w.win_rate >= 55.0 { Color::Green } else { Color::Yellow };

                vec![
                    Line::from(vec![
                        Span::styled("  Address: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(addr_short, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                        Span::raw("  │  Trades: "),
                        Span::styled(format!("{}", w.trade_count), Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from(vec![
                        Span::styled("  Winrate: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(bar, Style::default().fg(wr_color)),
                        Span::raw(" "),
                        Span::styled(format!("{:.1}%", w.win_rate * 100.0), Style::default().fg(wr_color).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(vec![Span::raw("")]),
                ]
            }).collect();

            let whale_info = Paragraph::new(whale_lines)
                .block(Block::default()
                    .title(" 🐋 TARGET WHALE ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Magenta)));
            frame.render_widget(whale_info, right_chunks[2]);

            // ── Footer / Risk Bar ──
            let loss_pct = if loss_halt > 0 {
                (losses as f64 / loss_halt as f64 * 100.0).min(100.0) as u16
            } else { 0 };

            let risk_gauge = Gauge::default()
                .block(Block::default()
                    .title(format!(" Risk │ Losses: {}/{} │ {} │ [P]ause  [Q]uit ",
                        losses, loss_halt,
                        if losses >= loss_halt { "⚠ HALTED" } else { "OK" }
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(
                        if losses >= loss_halt { Color::Red } else { Color::DarkGray }
                    )))
                .gauge_style(Style::default().fg(
                    if loss_pct > 66 { Color::Red } else if loss_pct > 33 { Color::Yellow } else { Color::Green }
                ))
                .ratio(loss_pct as f64 / 100.0)
                .label(format!("{}%", loss_pct));
            frame.render_widget(risk_gauge, main_chunks[3]);
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
