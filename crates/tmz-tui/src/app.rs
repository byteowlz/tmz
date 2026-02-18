//! Application state and main loop.

use crate::event::{self, Event};
use crate::ui;
use anyhow::Result;
use crossterm::{
    event::KeyEventKind,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tmz_core::{AppConfig, AppPaths, CachedConversation, CachedMessage};

// ─── Focus & Mode ────────────────────────────────────────────────────

/// Which panel has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    ChatList,
    Messages,
    Input,
    Files,
}

/// Current input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Search,
    Help,
    ChatSearch,
}

/// Left panel tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideTab {
    Chats,
    Teams,
    Channels,
}

// ─── App State ───────────────────────────────────────────────────────

pub struct App {
    pub config: AppConfig,
    pub mode: Mode,
    pub focus: Focus,
    pub side_tab: SideTab,
    pub running: bool,

    // Conversation list
    pub conversations: Vec<CachedConversation>,
    pub filtered_conversations: Vec<usize>,
    pub chat_selected: usize,
    pub chat_search: String,

    // Messages
    pub messages: Vec<CachedMessage>,
    pub msg_scroll: usize,

    // Input
    pub input: String,
    pub cursor_pos: usize,

    // In-chat search
    pub search_query: String,
    pub search_results: Vec<usize>,

    // Files panel
    pub show_files: bool,

    // Sync state
    pub last_sync: Option<Instant>,
    pub syncing: bool,
    pub token_expires_mins: Option<i64>,
    pub status_msg: String,

    // Cache
    pub cache: Option<tmz_core::Cache>,
}

impl App {
    pub const fn new(config: AppConfig) -> Self {
        Self {
            config,
            mode: Mode::Normal,
            focus: Focus::ChatList,
            side_tab: SideTab::Chats,
            running: true,

            conversations: Vec::new(),
            filtered_conversations: Vec::new(),
            chat_selected: 0,
            chat_search: String::new(),

            messages: Vec::new(),
            msg_scroll: 0,

            input: String::new(),
            cursor_pos: 0,

            search_query: String::new(),
            search_results: Vec::new(),

            show_files: false,

            last_sync: None,
            syncing: false,
            token_expires_mins: None,
            status_msg: String::new(),

            cache: None,
        }
    }

    /// Get the currently selected conversation.
    pub fn selected_conversation(&self) -> Option<&CachedConversation> {
        let idx = *self.filtered_conversations.get(self.chat_selected)?;
        self.conversations.get(idx)
    }

    /// Filter conversations by the current search string.
    pub fn filter_conversations(&mut self) {
        if self.chat_search.is_empty() {
            self.filtered_conversations = (0..self.conversations.len()).collect();
        } else {
            let query = self.chat_search.to_lowercase();
            self.filtered_conversations = self
                .conversations
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    c.display_name.to_lowercase().contains(&query)
                        || c.member_names.to_lowercase().contains(&query)
                })
                .map(|(i, _)| i)
                .collect();
        }
        // Clamp selection
        if self.chat_selected >= self.filtered_conversations.len() {
            self.chat_selected = self.filtered_conversations.len().saturating_sub(1);
        }
    }

    pub const fn chat_list_len(&self) -> usize {
        self.filtered_conversations.len()
    }

    pub fn chat_next(&mut self) {
        let len = self.chat_list_len();
        if len > 0 {
            self.chat_selected = (self.chat_selected + 1).min(len - 1);
        }
    }

    pub const fn chat_prev(&mut self) {
        self.chat_selected = self.chat_selected.saturating_sub(1);
    }

    pub const fn msg_scroll_down(&mut self) {
        self.msg_scroll = self.msg_scroll.saturating_add(3);
    }

    pub const fn msg_scroll_up(&mut self) {
        self.msg_scroll = self.msg_scroll.saturating_sub(3);
    }

    pub const fn msg_scroll_bottom(&mut self) {
        // Will be clamped during render
        self.msg_scroll = usize::MAX;
    }

    pub fn input_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn input_backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i);
            self.input.replace_range(prev..self.cursor_pos, "");
            self.cursor_pos = prev;
        }
    }

    pub fn input_clear(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
    }
}

// ─── Main loop ───────────────────────────────────────────────────────

pub fn run(config_path: Option<&PathBuf>) -> Result<()> {
    let paths = AppPaths::discover(config_path.map(PathBuf::as_path))?;
    let config = AppConfig::load(&paths, false)?;

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);

    // Load cache
    let rt = tokio::runtime::Runtime::new()?;
    let cache_dir = tmz_core::default_data_dir()?;
    let db_path = cache_dir.join("cache.db");
    let cache = rt.block_on(tmz_core::Cache::open(&db_path))?;

    // Initial load
    app.conversations = rt.block_on(cache.list_conversations(500))?;
    app.filter_conversations();

    // Load messages for first conversation
    if let Some(conv) = app.selected_conversation() {
        let id = conv.id.clone();
        app.messages = rt.block_on(cache.get_messages(&id, 200))?;
        app.msg_scroll_bottom();
    }

    // Check token status
    if let Ok(auth) = tmz_core::AuthManager::new()
        && let Ok(tokens) = auth.get_tokens()
    {
        let remaining = tokens.expires_at - chrono::Utc::now().timestamp();
        app.token_expires_mins = Some(remaining / 60);
    }

    app.cache = Some(cache);
    app.last_sync = Some(Instant::now());
    app.status_msg = format!("{} conversations loaded", app.conversations.len());

    // Event loop
    let events = event::spawn_event_reader(Duration::from_millis(200));

    while app.running {
        terminal.draw(|f| ui::draw(f, &app))?;

        match events.recv()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key, &rt);
                }
            }
            Event::Resize => {} // ratatui handles this
            Event::Tick => {
                handle_tick(&mut app, &rt);
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    rt: &tokio::runtime::Runtime,
) {
    match app.mode {
        Mode::Normal => handle_normal_key(app, key, rt),
        Mode::Insert => handle_insert_key(app, key, rt),
        Mode::ChatSearch => handle_chat_search_key(app, key, rt),
        Mode::Search => handle_search_key(app, key),
        Mode::Help => {
            if matches!(
                key.code,
                crossterm::event::KeyCode::Esc
                    | crossterm::event::KeyCode::Char('q' | '?')
            ) {
                app.mode = Mode::Normal;
            }
        }
    }
}

fn handle_normal_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    rt: &tokio::runtime::Runtime,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Char('q') => app.running = false,
        KeyCode::Char('?') => app.mode = Mode::Help,

        // Focus switching
        KeyCode::Char('h') | KeyCode::Left => app.focus = Focus::ChatList,
        KeyCode::Char('l') | KeyCode::Right => {
            if app.focus == Focus::ChatList {
                app.focus = Focus::Messages;
            } else if app.focus == Focus::Messages {
                app.focus = Focus::Files;
                app.show_files = true;
            }
        }
        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::ChatList => Focus::Messages,
                Focus::Messages => Focus::Input,
                Focus::Input | Focus::Files => Focus::ChatList,
            };
        }
        KeyCode::BackTab => {
            app.focus = match app.focus {
                Focus::ChatList => Focus::Input,
                Focus::Messages => Focus::ChatList,
                Focus::Input | Focus::Files => Focus::Messages,
            };
        }

        // Navigation
        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Focus::ChatList => {
                app.chat_next();
                load_selected_chat(app, rt);
            }
            Focus::Messages => app.msg_scroll_down(),
            _ => {}
        },
        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Focus::ChatList => {
                app.chat_prev();
                load_selected_chat(app, rt);
            }
            Focus::Messages => app.msg_scroll_up(),
            _ => {}
        },
        KeyCode::Char('G') if app.focus == Focus::Messages => {
            app.msg_scroll_bottom();
        }
        KeyCode::Char('g') if app.focus == Focus::Messages => {
            app.msg_scroll = 0;
        }

        // Enter insert mode
        KeyCode::Char('i') | KeyCode::Enter => {
            app.mode = Mode::Insert;
            app.focus = Focus::Input;
        }

        // Chat search (fuzzy find)
        KeyCode::Char('/') => {
            if app.focus == Focus::ChatList {
                app.mode = Mode::ChatSearch;
                app.chat_search.clear();
            } else if app.focus == Focus::Messages {
                app.mode = Mode::Search;
                app.search_query.clear();
            }
        }

        // Side tabs
        KeyCode::Char('1') => app.side_tab = SideTab::Chats,
        KeyCode::Char('2') => app.side_tab = SideTab::Teams,
        KeyCode::Char('3') => app.side_tab = SideTab::Channels,

        // Toggle files panel
        KeyCode::Char('f') => app.show_files = !app.show_files,

        // Sync
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            trigger_sync(app, rt);
        }

        _ => {}
    }
}

fn handle_insert_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    rt: &tokio::runtime::Runtime,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.focus = Focus::Messages;
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                send_message(app, rt);
            }
        }
        KeyCode::Backspace => app.input_backspace(),
        KeyCode::Char(c) => app.input_char(c),
        _ => {}
    }
}

fn handle_chat_search_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    rt: &tokio::runtime::Runtime,
) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.chat_search.clear();
            app.filter_conversations();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            if !app.filtered_conversations.is_empty() {
                load_selected_chat(app, rt);
                app.focus = Focus::Messages;
            }
        }
        KeyCode::Backspace => {
            app.chat_search.pop();
            app.filter_conversations();
        }
        KeyCode::Char(c) => {
            app.chat_search.push(c);
            app.chat_selected = 0;
            app.filter_conversations();
        }
        KeyCode::Down => app.chat_next(),
        KeyCode::Up => app.chat_prev(),
        _ => {}
    }
}

fn handle_search_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            app.search_query.clear();
            app.search_results.clear();
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            app.search_query.pop();
        }
        KeyCode::Char(c) => {
            app.search_query.push(c);
        }
        _ => {}
    }
}

fn handle_tick(app: &mut App, rt: &tokio::runtime::Runtime) {
    // Auto-sync every 60 seconds
    if let Some(last) = app.last_sync
        && last.elapsed() > Duration::from_secs(60)
        && !app.syncing
    {
        if let Some(ref cache) = app.cache
            && let Ok(convs) = rt.block_on(cache.list_conversations(500))
        {
            let selected_id = app.selected_conversation().map(|c| c.id.clone());
            app.conversations = convs;
            app.filter_conversations();

            if let Some(id) = selected_id
                && let Some(pos) = app
                    .filtered_conversations
                    .iter()
                    .position(|&i| app.conversations[i].id == id)
            {
                app.chat_selected = pos;
            }
        }
        app.last_sync = Some(Instant::now());
    }

    // Update token expiry
    if let Ok(auth) = tmz_core::AuthManager::new()
        && let Ok(tokens) = auth.get_tokens()
    {
        let remaining = tokens.expires_at - chrono::Utc::now().timestamp();
        app.token_expires_mins = Some(remaining / 60);
    }
}

fn load_selected_chat(app: &mut App, rt: &tokio::runtime::Runtime) {
    if let Some(conv) = app.selected_conversation() {
        let id = conv.id.clone();
        if let Some(ref cache) = app.cache
            && let Ok(msgs) = rt.block_on(cache.get_messages(&id, 200))
        {
            app.messages = msgs;
            app.msg_scroll_bottom();
        }
    }
}

fn send_message(app: &mut App, rt: &tokio::runtime::Runtime) {
    let Some(conv) = app.selected_conversation() else {
        return;
    };
    let conv_id = conv.id.clone();
    let text = app.input.clone();
    app.input_clear();

    match tmz_core::TeamsClient::new() {
        Ok(client) => match rt.block_on(client.send_message(&conv_id, &text)) {
            Ok(_) => {
                app.status_msg = "Sent".to_string();
                load_selected_chat(app, rt);
            }
            Err(e) => {
                app.status_msg = format!("Send failed: {e}");
            }
        },
        Err(e) => {
            app.status_msg = format!("Not connected: {e}");
        }
    }
}

fn trigger_sync(app: &mut App, rt: &tokio::runtime::Runtime) {
    app.status_msg = "Syncing...".to_string();
    app.syncing = true;

    match tmz_core::TeamsClient::new() {
        Ok(client) => {
            if let Ok(data) = rt.block_on(client.list_chats()) {
                if let Some(ref cache) = app.cache {
                    if let Some(convs) = data["conversations"].as_array() {
                        for conv in convs {
                            let cached = tmz_core::cache::parse_conversation(conv);
                            let _ = rt.block_on(cache.upsert_conversation(&cached));
                        }
                    }
                    if let Ok(convs) = rt.block_on(cache.list_conversations(500)) {
                        app.conversations = convs;
                        app.filter_conversations();
                    }
                }
                app.status_msg = "Synced".to_string();
            } else {
                app.status_msg = "Sync failed".to_string();
            }
        }
        Err(e) => {
            app.status_msg = format!("Sync failed: {e}");
        }
    }

    app.syncing = false;
    app.last_sync = Some(Instant::now());
}
