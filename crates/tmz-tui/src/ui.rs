//! UI rendering.

use crate::app::{App, Focus, Mode, SideTab};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

// ─── Colors ──────────────────────────────────────────────────────────

const ACCENT: Color = Color::Rgb(88, 101, 242); // Discord-like indigo
const SELF_COLOR: Color = Color::Cyan;
const OTHER_COLOR: Color = Color::Yellow;
const DIM: Color = Color::DarkGray;
const BG_SELECTED: Color = Color::Rgb(40, 40, 50);
const BG_INPUT: Color = Color::Rgb(30, 30, 40);
const SEARCH_HIGHLIGHT: Color = Color::Rgb(255, 180, 0);

// ─── Main draw ───────────────────────────────────────────────────────

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    let main_area = outer[0];
    let status_area = outer[1];

    // Main 3-panel layout (or 2 if files hidden)
    let constraints = if app.show_files {
        vec![
            Constraint::Length(28),
            Constraint::Min(40),
            Constraint::Length(30),
        ]
    } else {
        vec![Constraint::Length(28), Constraint::Min(40)]
    };

    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(main_area);

    draw_chat_list(f, app, panels[0]);

    // Split middle into messages + input bar
    let middle = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(panels[1]);

    draw_messages(f, app, middle[0]);
    draw_input(f, app, middle[1]);

    if app.show_files && panels.len() > 2 {
        draw_files(f, app, panels[2]);
    }

    draw_status_bar(f, app, status_area);

    // Overlays
    if matches!(app.mode, Mode::Help) {
        draw_help(f);
    }
}

// ─── Left panel: chat list ───────────────────────────────────────────

fn draw_chat_list(f: &mut Frame<'_>, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::ChatList;
    let border_style = if is_focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    // Tab header
    let tab_line = Line::from(vec![
        tab_span("chats", app.side_tab == SideTab::Chats, is_focused),
        Span::raw(" "),
        tab_span("teams", app.side_tab == SideTab::Teams, is_focused),
        Span::raw(" "),
        tab_span("chan", app.side_tab == SideTab::Channels, is_focused),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .padding(Padding::horizontal(1));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // Layout: tabs + search + list
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Length(1), // search
            Constraint::Min(0),   // list
        ])
        .split(inner);

    f.render_widget(Paragraph::new(tab_line), chunks[0]);

    // Search bar
    let search_style = if matches!(app.mode, Mode::ChatSearch) {
        Style::default().fg(SEARCH_HIGHLIGHT)
    } else {
        Style::default().fg(DIM)
    };
    let search_text = if app.chat_search.is_empty() && !matches!(app.mode, Mode::ChatSearch) {
        " / search...".to_string()
    } else {
        format!(" /{}", app.chat_search)
    };
    f.render_widget(
        Paragraph::new(search_text).style(search_style),
        chunks[1],
    );

    // Conversation list
    let items: Vec<ListItem<'_>> = app
        .filtered_conversations
        .iter()
        .enumerate()
        .map(|(i, &conv_idx)| {
            let conv = &app.conversations[conv_idx];
            let name = if conv.display_name.is_empty() {
                &conv.id
            } else {
                &conv.display_name
            };
            let truncated: String = name.chars().take(24).collect();

            let preview: String = conv
                .last_message_preview
                .chars()
                .take(22)
                .collect::<String>()
                .replace('\n', " ");

            let is_selected = i == app.chat_selected;
            let style = if is_selected {
                Style::default().bg(BG_SELECTED).fg(Color::White).bold()
            } else {
                Style::default().fg(Color::Gray)
            };
            let preview_style = if is_selected {
                Style::default().bg(BG_SELECTED).fg(DIM)
            } else {
                Style::default().fg(DIM)
            };

            ListItem::new(vec![
                Line::from(Span::styled(truncated, style)),
                Line::from(Span::styled(format!(" {preview}"), preview_style)),
            ])
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[2]);
}

fn tab_span(label: &str, active: bool, focused: bool) -> Span<'_> {
    if active {
        let color = if focused { ACCENT } else { Color::White };
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(label, Style::default().fg(DIM))
    }
}

// ─── Middle panel: messages ──────────────────────────────────────────

fn draw_messages(f: &mut Frame<'_>, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Messages;
    let border_style = if is_focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let title = app.selected_conversation().map_or_else(
        || " messages ".to_string(),
        |c| {
            if c.display_name.is_empty() {
                " messages ".to_string()
            } else {
                format!(" {} ", c.display_name)
            }
        },
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.messages.is_empty() {
        let empty = Paragraph::new("  No messages. Select a chat or sync first.")
            .style(Style::default().fg(DIM));
        f.render_widget(empty, inner);
        return;
    }

    let lines = build_message_lines(&app.messages);
    let total_lines = lines.len();
    let visible = inner.height as usize;
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.msg_scroll.min(max_scroll);

    let para = Paragraph::new(lines)
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    if total_lines > visible {
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(DIM)),
            inner,
            &mut scrollbar_state,
        );
    }
}

fn build_message_lines<'a>(messages: &'a [tmz_core::CachedMessage]) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut prev_sender: Option<&str> = None;
    let mut prev_date: Option<String> = None;

    for msg in messages {
        let date = msg.compose_time.split('T').next().unwrap_or("");
        if prev_date.as_deref() != Some(date) {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            let label = format_date(date);
            lines.push(Line::from(Span::styled(
                format!(" -- {label} --"),
                Style::default().fg(DIM),
            )));
            lines.push(Line::from(""));
            prev_sender = None;
            prev_date = Some(date.to_string());
        }

        let sender = &msg.from_display_name;
        let time = extract_time(&msg.compose_time);
        let is_me = msg.is_from_me;

        if prev_sender != Some(sender.as_str()) {
            if prev_sender.is_some() {
                lines.push(Line::from(""));
            }
            let color = if is_me { SELF_COLOR } else { OTHER_COLOR };
            lines.push(Line::from(vec![
                Span::styled("  | ", Style::default().fg(color)),
                Span::styled(
                    sender.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {time}"), Style::default().fg(DIM)),
            ]));
            prev_sender = Some(sender.as_str());
        }

        let color = if is_me { SELF_COLOR } else { OTHER_COLOR };
        let content = if msg.content.is_empty() {
            "[image]"
        } else {
            &msg.content
        };

        for text_line in content.lines() {
            if !text_line.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  | ", Style::default().fg(color)),
                    Span::styled(text_line.to_string(), Style::default().fg(Color::White)),
                ]));
            }
        }
    }

    lines
}

// ─── Input bar ───────────────────────────────────────────────────────

fn draw_input(f: &mut Frame<'_>, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Input || matches!(app.mode, Mode::Insert);
    let border_style = if is_focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(Style::default().bg(BG_INPUT));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let display = if app.input.is_empty() && !is_focused {
        Paragraph::new("  Type a message... (i)")
            .style(Style::default().fg(DIM))
    } else {
        Paragraph::new(format!("  {}", app.input))
            .style(Style::default().fg(Color::White))
    };

    f.render_widget(display, inner);

    // Show cursor in insert mode
    if is_focused {
        let x = inner.x + 2 + app.cursor_pos as u16;
        let y = inner.y;
        f.set_cursor_position((x, y));
    }
}

// ─── Right panel: files ──────────────────────────────────────────────

fn draw_files(f: &mut Frame<'_>, app: &App, area: Rect) {
    let is_focused = app.focus == Focus::Files;
    let border_style = if is_focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    };

    let block = Block::default()
        .title(" files ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    // TODO: populate from chat metadata / search for file messages
    let placeholder = Paragraph::new("  No files")
        .style(Style::default().fg(DIM));
    f.render_widget(placeholder, inner);
}

// ─── Status bar ──────────────────────────────────────────────────────

fn draw_status_bar(f: &mut Frame<'_>, app: &App, area: Rect) {
    let mode_span = match app.mode {
        Mode::Normal => Span::styled(
            " NORMAL ",
            Style::default().fg(Color::Black).bg(ACCENT).bold(),
        ),
        Mode::Insert => Span::styled(
            " INSERT ",
            Style::default().fg(Color::Black).bg(Color::Green).bold(),
        ),
        Mode::Search => Span::styled(
            " SEARCH ",
            Style::default()
                .fg(Color::Black)
                .bg(SEARCH_HIGHLIGHT)
                .bold(),
        ),
        Mode::ChatSearch => Span::styled(
            " FIND ",
            Style::default()
                .fg(Color::Black)
                .bg(SEARCH_HIGHLIGHT)
                .bold(),
        ),
        Mode::Help => Span::styled(
            " HELP ",
            Style::default().fg(Color::Black).bg(Color::Yellow).bold(),
        ),
    };

    let token_span = match app.token_expires_mins {
        Some(mins) if mins > 10 => Span::styled(
            format!(" {mins}m "),
            Style::default().fg(Color::Green),
        ),
        Some(mins) if mins > 0 => Span::styled(
            format!(" {mins}m "),
            Style::default().fg(Color::Yellow),
        ),
        Some(_) => Span::styled(" expired ", Style::default().fg(Color::Red)),
        None => Span::styled(" no auth ", Style::default().fg(Color::Red)),
    };

    let sync_span = if app.syncing {
        Span::styled(" syncing... ", Style::default().fg(Color::Yellow))
    } else if let Some(last) = app.last_sync {
        let ago = last.elapsed().as_secs();
        if ago < 60 {
            Span::styled(" synced ", Style::default().fg(Color::Green))
        } else {
            Span::styled(
                format!(" {}m ago ", ago / 60),
                Style::default().fg(DIM),
            )
        }
    } else {
        Span::styled(" not synced ", Style::default().fg(DIM))
    };

    let status = Span::styled(
        format!(" {} ", app.status_msg),
        Style::default().fg(DIM),
    );

    let profile = Span::styled(
        format!(" [{}] ", app.config.profile),
        Style::default().fg(DIM),
    );

    let keys_hint = Span::styled(
        " ? help  / search  i msg  q quit ",
        Style::default().fg(DIM),
    );

    let line = Line::from(vec![
        mode_span,
        Span::raw(" "),
        token_span,
        Span::raw("│"),
        sync_span,
        Span::raw("│"),
        status,
        profile,
        Span::raw(" "),
        keys_hint,
    ]);

    f.render_widget(
        Paragraph::new(line).alignment(Alignment::Left),
        area,
    );
}

// ─── Help overlay ────────────────────────────────────────────────────

fn draw_help(f: &mut Frame<'_>) {
    let area = centered_rect(50, 70, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" keybindings ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Rgb(25, 25, 35)));

    let help = vec![
        Line::from(""),
        section("navigation"),
        key("j / k", "move up / down"),
        key("h / l", "focus left / right panel"),
        key("Tab", "cycle focus forward"),
        key("Shift+Tab", "cycle focus backward"),
        key("g / G", "scroll to top / bottom"),
        Line::from(""),
        section("actions"),
        key("i / Enter", "start typing a message"),
        key("Esc", "back to normal mode"),
        key("/", "search (chats or messages)"),
        key("f", "toggle files panel"),
        key("Ctrl+r", "sync now"),
        key("1 2 3", "switch tabs: chats / teams / channels"),
        Line::from(""),
        section("general"),
        key("?", "toggle this help"),
        key("q", "quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  press ? or Esc to close",
            Style::default().fg(DIM),
        )),
    ];

    let para = Paragraph::new(help).block(block);
    f.render_widget(para, area);
}

fn section(name: &str) -> Line<'_> {
    Line::from(Span::styled(
        format!("  {name}"),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))
}

fn key<'a>(keys: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {keys:<16}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(Color::Gray)),
    ])
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn extract_time(compose_time: &str) -> String {
    // "2026-02-18T09:38:22.933Z" -> "09:38"
    compose_time
        .split('T')
        .nth(1)
        .and_then(|t| t.get(..5))
        .unwrap_or("??:??")
        .to_string()
}

fn format_date(date_str: &str) -> String {
    // "2026-02-18" -> "February 18, 2026"
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map_or_else(|_| date_str.to_string(), |d| d.format("%B %d, %Y").to_string())
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}
