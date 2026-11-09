use crate::device::{CorsairMouse, MouseStatus};
use crate::protocol::{
    compute_checksum, DEFAULT_DEBOUNCE_MS, FLASH_ADDR_DEBOUNCE, MAX_SAFE_DEBOUNCE_MS,
};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

/// Delay after the last adjustment keypress before automatically writing the new debounce value to flash.
const AUTO_APPLY_DELAY: Duration = Duration::from_millis(350);

/// Interval between automatic background status polls (battery, online state, profile).
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

struct TuiApp {
    mouse: CorsairMouse,
    status: MouseStatus,
    target_debounce_ms: u8,
    force_range: bool,
    pending_apply_deadline: Option<Instant>,
    last_refresh: Instant,
    status_message: String,
    is_error_message: bool,
}

impl TuiApp {
    fn new(mut mouse: CorsairMouse, force_range: bool) -> Result<Self> {
        let status = mouse.query_full_status()?;
        let initial_debounce = status
            .debounce
            .map(|d| d.debounce_ms)
            .unwrap_or(DEFAULT_DEBOUNCE_MS);

        let status_message = if status.online {
            format!(
                "Connected {}. Current debounce: {} ms.",
                status.connection_kind.status_phrase(),
                initial_debounce
            )
        } else {
            "Wireless receiver connected, but mouse is asleep/offline. Move or click the mouse to wake it.".to_string()
        };

        Ok(Self {
            mouse,
            status,
            target_debounce_ms: initial_debounce,
            force_range,
            pending_apply_deadline: None,
            last_refresh: Instant::now(),
            status_message,
            is_error_message: false,
        })
    }

    fn max_allowed(&self) -> u8 {
        if self.force_range {
            u8::MAX
        } else {
            MAX_SAFE_DEBOUNCE_MS
        }
    }

    fn schedule_apply(&mut self) {
        let current_applied = self.status.debounce.map(|d| d.debounce_ms);
        if current_applied != Some(self.target_debounce_ms) {
            self.pending_apply_deadline = Some(Instant::now() + AUTO_APPLY_DELAY);
            self.is_error_message = false;
            self.status_message = format!(
                "Selected {} ms — applying to mouse...",
                self.target_debounce_ms
            );
        } else {
            self.pending_apply_deadline = None;
            self.is_error_message = false;
            self.status_message = format!(
                "Debounce is already set to {} ms.",
                self.target_debounce_ms
            );
        }
    }

    fn adjust_target(&mut self, delta: i16) {
        let max_val = self.max_allowed() as i16;
        let next = (self.target_debounce_ms as i16 + delta).clamp(0, max_val) as u8;
        self.target_debounce_ms = next;
        self.schedule_apply();
    }

    fn set_target(&mut self, val: u8) {
        self.target_debounce_ms = val.min(self.max_allowed());
        self.schedule_apply();
    }

    fn toggle_force_range(&mut self) {
        self.force_range = !self.force_range;
        if !self.force_range && self.target_debounce_ms > MAX_SAFE_DEBOUNCE_MS {
            self.target_debounce_ms = MAX_SAFE_DEBOUNCE_MS;
            self.schedule_apply();
        }
        self.is_error_message = false;
        self.status_message = if self.force_range {
            "Unlocked extended debounce range (0..=255 ms).".to_string()
        } else {
            format!(
                "Locked to standard debounce range (0..={} ms).",
                MAX_SAFE_DEBOUNCE_MS
            )
        };
    }

    fn refresh(&mut self, manual: bool) {
        self.last_refresh = Instant::now();
        match self.mouse.query_full_status() {
            Ok(new_status) => {
                if self.pending_apply_deadline.is_none() {
                    if let Some(d) = new_status.debounce {
                        self.target_debounce_ms = d.debounce_ms;
                    }
                }
                self.status = new_status;
                if manual {
                    self.is_error_message = false;
                    self.status_message = "Refreshed device status and flash state.".to_string();
                }
            }
            Err(err) => {
                if manual {
                    self.is_error_message = true;
                    self.status_message = format!("Refresh failed: {}", err);
                }
            }
        }
    }

    fn apply_debounce(&mut self) {
        self.pending_apply_deadline = None;
        let target = self.target_debounce_ms;
        match self.mouse.write_debounce(target) {
            Ok(reading) => {
                if let Ok(new_status) = self.mouse.query_full_status() {
                    self.status = new_status;
                } else {
                    self.status.debounce = Some(reading);
                }
                self.last_refresh = Instant::now();
                self.is_error_message = false;
                self.status_message = format!(
                    "Saved {} ms debounce to mouse flash (0x{:04X} = 0x{:02X}, check 0x{:02X}).",
                    reading.debounce_ms, FLASH_ADDR_DEBOUNCE, reading.raw_value, reading.raw_check_byte
                );
            }
            Err(err) => {
                self.is_error_message = true;
                self.status_message = format!("Failed to write debounce time: {}", err);
            }
        }
    }

    fn on_tick(&mut self) {
        if let Some(deadline) = self.pending_apply_deadline {
            if Instant::now() >= deadline {
                self.apply_debounce();
                return;
            }
        }

        if self.pending_apply_deadline.is_none()
            && self.last_refresh.elapsed() >= AUTO_REFRESH_INTERVAL
        {
            self.refresh(false);
        }
    }
}

/// Launches the interactive Terminal User Interface.
pub fn run_tui(mouse: CorsairMouse, force_range: bool) -> Result<()> {
    let mut app = TuiApp::new(mouse, force_range)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let res = run_event_loop(&mut terminal, &mut app);

    let _ = app.mouse.set_pc_driver_status(false);
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    res
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> Result<()> {
    loop {
        app.on_tick();
        terminal.draw(|frame| draw_ui(frame, app))?;

        let poll_timeout = if let Some(deadline) = app.pending_apply_deadline {
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50))
        } else {
            Duration::from_millis(100)
        };

        if event::poll(poll_timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if app.pending_apply_deadline.is_some() {
                            app.apply_debounce();
                        }
                        return Ok(());
                    }
                    KeyCode::Left
                    | KeyCode::Down
                    | KeyCode::Char('h')
                    | KeyCode::Char('j')
                    | KeyCode::Char('-') => {
                        app.adjust_target(-1);
                    }
                    KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Char('l')
                    | KeyCode::Char('k')
                    | KeyCode::Char('+')
                    | KeyCode::Char('=') => {
                        app.adjust_target(1);
                    }
                    KeyCode::PageDown | KeyCode::Char('[') => {
                        app.adjust_target(-5);
                    }
                    KeyCode::PageUp | KeyCode::Char(']') => {
                        app.adjust_target(5);
                    }
                    KeyCode::Home => app.set_target(0),
                    KeyCode::End => app.set_target(app.max_allowed()),
                    KeyCode::Char(c @ '0'..='9') => {
                        let digit = (c as u8) - b'0';
                        app.set_target(digit);
                    }
                    KeyCode::Char('t') => app.set_target(10),
                    KeyCode::Char('f') => app.toggle_force_range(),
                    KeyCode::Char('r') => app.refresh(true),
                    KeyCode::Enter | KeyCode::Char(' ') => app.apply_debounce(),
                    _ => {}
                }
            }
        }
    }
}

fn draw_ui(frame: &mut Frame, app: &TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, chunks[0]);
    draw_device_status(frame, chunks[1], app);
    draw_debounce_control(frame, chunks[2], app);
    draw_protocol_trace(frame, chunks[3], app);
    draw_footer(frame, chunks[4], app);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " CORSAIR SABRE v2 PRO Debounce Utility ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("— Cross-Platform USB HID Controller"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

fn draw_device_status(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let online_span = if app.status.online {
        Span::styled(
            "ONLINE",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "OFFLINE / ASLEEP",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        )
    };

    let profile_text = app
        .status
        .active_profile
        .map(|idx| format!("Profile {} (slot {})", idx.saturating_add(1), idx))
        .unwrap_or_else(|| "N/A".to_string());

    let battery_text = app
        .status
        .battery
        .map(|b| {
            let state = if b.charging {
                "Charging"
            } else {
                "Discharging"
            };
            format!("{}% ({}, {} mV)", b.level_percent, state, b.voltage_mv)
        })
        .unwrap_or_else(|| "N/A".to_string());

    let applied_text = app
        .status
        .debounce
        .map(|d| {
            let validity = if d.checksum_valid {
                format!("check 0x{:02X} OK", d.raw_check_byte)
            } else {
                format!(
                    "uninitialized 0x{:02X}/0x{:02X}, default",
                    d.raw_value, d.raw_check_byte
                )
            };
            format!("{} ms ({})", d.debounce_ms, validity)
        })
        .unwrap_or_else(|| "N/A".to_string());

    let lines = vec![
        Line::from(vec![
            Span::styled(" Device:          ", Style::default().fg(Color::Cyan)),
            Span::raw(&app.status.product_name),
            Span::raw("  ["),
            online_span,
            Span::raw("]"),
        ]),
        Line::from(vec![
            Span::styled(" Transport:       ", Style::default().fg(Color::Cyan)),
            Span::raw(app.status.transport_description()),
        ]),
        Line::from(vec![
            Span::styled(" Active Profile:  ", Style::default().fg(Color::Cyan)),
            Span::raw(profile_text),
            Span::styled("    Battery: ", Style::default().fg(Color::Cyan)),
            Span::raw(battery_text),
        ]),
        Line::from(vec![
            Span::styled(" Stored Debounce: ", Style::default().fg(Color::Cyan)),
            Span::styled(
                applied_text,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let block =
        Paragraph::new(lines).block(Block::default().title(" Device Status ").borders(Borders::ALL));
    frame.render_widget(block, area);
}

fn draw_debounce_control(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let max_val = app.max_allowed().max(1);
    let check_byte = compute_checksum(&[app.target_debounce_ms]);
    let is_modified = app
        .status
        .debounce
        .map(|d| d.debounce_ms != app.target_debounce_ms)
        .unwrap_or(true);

    // Build a 20-cell visual slider bar that never clips regardless of terminal height.
    const BAR_WIDTH: usize = 20;
    let filled = ((app.target_debounce_ms as usize * BAR_WIDTH) / (max_val as usize)).min(BAR_WIDTH);
    let empty = BAR_WIDTH.saturating_sub(filled);
    let bar_filled = "█".repeat(filled);
    let bar_empty = "░".repeat(empty);

    let bar_color = if is_modified {
        Color::Yellow
    } else {
        Color::Green
    };

    let state_span = if is_modified {
        Span::styled(
            " [APPLYING / PRESS ENTER]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " [SAVED IN MOUSE]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    };

    let slider_line = Line::from(vec![
        Span::styled(" Target Debounce: ", Style::default().fg(Color::Cyan)),
        Span::styled("[", Style::default().fg(Color::White)),
        Span::styled(bar_filled, Style::default().fg(bar_color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled("] ", Style::default().fg(Color::White)),
        Span::styled(
            format!("{} ms", app.target_debounce_ms),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " / {} ms (0x{:02X}, check 0x{:02X})",
            max_val, app.target_debounce_ms, check_byte
        )),
        state_span,
    ]);

    let controls_line = Line::from(vec![
        Span::styled(
            " [Left/Right/Up/Down or -/+] ",
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("-/+ 1 ms   "),
        Span::styled("[PgDn/PgUp or [/]] ", Style::default().fg(Color::Yellow)),
        Span::raw("-/+ 5 ms   "),
        Span::styled("[0-9 / t] ", Style::default().fg(Color::Yellow)),
        Span::raw("0-9 / 10 ms"),
    ]);

    let actions_line = Line::from(vec![
        Span::styled(
            " [Enter / Space] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("Apply Now   "),
        Span::styled("[r] ", Style::default().fg(Color::Cyan)),
        Span::raw("Refresh   "),
        Span::styled("[f] ", Style::default().fg(Color::Magenta)),
        Span::raw(if app.force_range {
            "Lock 0-20 ms   "
        } else {
            "Unlock 0-255 ms   "
        }),
        Span::styled("[q / Esc] ", Style::default().fg(Color::Red)),
        Span::raw("Quit"),
    ]);

    let lines = vec![
        slider_line,
        Line::from(""),
        controls_line,
        actions_line,
    ];

    let block = Paragraph::new(lines).block(
        Block::default()
            .title(" Debounce Timing Configuration (Auto-Saves to Flash 0x00A9) ")
            .borders(Borders::ALL),
    );
    frame.render_widget(block, area);
}

fn draw_protocol_trace(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let max_items = area.height.saturating_sub(2) as usize;
    let start_idx = app.mouse.trace_log.len().saturating_sub(max_items);
    let items: Vec<ListItem> = app.mouse.trace_log[start_idx..]
        .iter()
        .map(|entry| ListItem::new(Line::from(Span::raw(format!(" {}", entry)))))
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" 17-Byte HID Report Trace (Report ID 0x08) ")
            .borders(Borders::ALL),
    );
    frame.render_widget(list, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &TuiApp) {
    let style = if app.is_error_message {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let footer = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(&app.status_message, style),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}
