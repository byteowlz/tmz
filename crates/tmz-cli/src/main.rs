//! CLI interface for tmz - Microsoft Teams from the terminal.

use std::env;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use env_logger::fmt::WriteStyle;
use log::{LevelFilter, debug};
use tmz_core::cache::{self, Cache};
use tmz_core::paths::write_default_config;
use tmz_core::{AppConfig, AppPaths, AuthManager, TeamsClient, default_cache_dir};

const APP_NAME: &str = "tmz";

fn main() -> anyhow::Result<()> {
    try_main()
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();

    let ctx = RuntimeContext::new(cli.common.clone())?;
    ctx.init_logging()?;
    debug!("resolved paths: {:#?}", ctx.paths);

    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        Command::Auth { subcommand } => rt.block_on(handle_auth(&ctx, subcommand)),
        Command::Sync(cmd) => rt.block_on(handle_sync(&ctx, cmd)),
        Command::Chats(cmd) => rt.block_on(handle_chats(&ctx, cmd)),
        Command::Msg {
            target,
            message,
            file,
            limit,
            no_images,
        } => rt.block_on(handle_msg(&ctx, target, message, file, limit, no_images)),
        Command::Search { query, chat, limit } => {
            rt.block_on(handle_search(&ctx, &query, chat.as_deref(), limit))
        }
        Command::Find { query, conv_type } => rt.block_on(handle_find(&ctx, &query, conv_type)),
        Command::Alias {
            name,
            target,
            conv_type,
        } => rt.block_on(handle_alias(&ctx, &name, target, conv_type)),
        Command::Teams { subcommand } => rt.block_on(handle_teams(&ctx, subcommand)),
        Command::Service { command } => rt.block_on(handle_service(&ctx, command)),
        Command::Init(cmd) => handle_init(&ctx, cmd),
        Command::Config { command } => handle_config(&ctx, command),
        Command::Completions { shell } => {
            handle_completions(shell);
            Ok(())
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "tmz",
    author,
    version,
    about = "Microsoft Teams from the terminal",
    propagate_version = true
)]
struct Cli {
    #[command(flatten)]
    common: CommonOpts,
    #[command(subcommand)]
    command: Command,
}

/// Common CLI options shared across all subcommands.
#[derive(Debug, Clone, Args)]
pub struct CommonOpts {
    /// Override the config file path.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
    /// Reduce output to only errors.
    #[arg(short, long, action = clap::ArgAction::SetTrue, global = true)]
    pub quiet: bool,
    /// Increase logging verbosity (stackable).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Enable debug logging.
    #[arg(long, global = true)]
    pub debug: bool,
    /// Enable trace logging.
    #[arg(long, global = true)]
    pub trace: bool,
    /// Output machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,
    /// Disable ANSI colors in output.
    #[arg(long = "no-color", global = true, conflicts_with = "color")]
    pub no_color: bool,
    /// Control color output.
    #[arg(long, value_enum, default_value_t = ColorOption::Auto, global = true)]
    pub color: ColorOption,
    /// Do not change anything on disk.
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,
    /// Assume "yes" for interactive prompts.
    #[arg(short = 'y', long = "yes", alias = "force", global = true)]
    pub assume_yes: bool,
}

/// Color output mode.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ColorOption {
    /// Detect terminal capabilities automatically.
    Auto,
    /// Always emit ANSI color codes.
    Always,
    /// Never emit ANSI color codes.
    Never,
}

/// Filter conversations by type.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ConvTypeFilter {
    /// 1:1 direct messages.
    #[value(name = "1:1", alias = "dm", alias = "direct")]
    OneToOne,
    /// Group chats.
    #[value(alias = "grp")]
    Group,
    /// Teams channels.
    #[value(alias = "chan")]
    Channel,
    /// Meeting chats.
    #[value(alias = "meet")]
    Meeting,
}

impl ConvTypeFilter {
    /// Check if a conversation matches this filter based on its product type.
    ///
    /// Known product types from Teams API:
    /// - `OneToOneChat` - 1:1 direct messages
    /// - `Chat` - group chats
    /// - `Meeting` - meeting chat threads
    /// - `TeamsStandardChannel`, `TeamsPrivateChannel` - team channels
    /// - `TeamsTeam` - team container (usually not a chat target)
    /// - `SfbInteropChat` - Skype for Business interop
    /// - `Stream*` - notification/activity streams (not chats)
    fn matches(self, product_type: &str) -> bool {
        match self {
            Self::OneToOne => {
                product_type == "OneToOneChat" || product_type == "SfbInteropChat"
            }
            Self::Group => product_type == "Chat",
            Self::Channel => {
                product_type == "TeamsStandardChannel"
                    || product_type == "TeamsPrivateChannel"
                    || product_type == "TeamsTeam"
            }
            Self::Meeting => product_type == "Meeting",
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Authentication (login, logout, status).
    Auth {
        #[command(subcommand)]
        subcommand: AuthSubcommand,
    },
    /// Sync conversations and messages to local cache.
    Sync(SyncCommand),
    /// List cached conversations.
    Chats(ChatsCommand),
    /// Read or send messages. Usage: tmz msg <person> [message].
    Msg {
        /// Person alias, display name, or conversation ID.
        target: String,
        /// Message to send. Omit to show recent messages.
        message: Option<String>,
        /// Send a file instead of (or with) a message.
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Number of recent messages to show (default: 20).
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: i64,
        /// Disable inline image rendering (Kitty graphics protocol).
        #[arg(long)]
        no_images: bool,
    },
    /// Full-text search across cached messages.
    Search {
        /// Search query (FTS5 syntax).
        query: String,
        /// Scope to a specific chat (alias, name, or ID).
        #[arg(short, long, value_name = "CHAT")]
        chat: Option<String>,
        /// Max results.
        #[arg(short, long, default_value_t = 20)]
        limit: i64,
    },
    /// Find a conversation by name and show its ID.
    Find {
        /// Search term (fuzzy matched against names, members, IDs).
        query: String,
        /// Filter by conversation type: 1:1, group, channel, meeting.
        #[arg(short = 't', long = "type", value_enum)]
        conv_type: Option<ConvTypeFilter>,
    },
    /// Create a people/chat alias (written to config.toml).
    Alias {
        /// Short alias name (e.g., "alex").
        name: String,
        /// Conversation ID or search term. Omit to search interactively.
        target: Option<String>,
        /// Filter by conversation type: 1:1, group, channel, meeting.
        #[arg(short = 't', long = "type", value_enum)]
        conv_type: Option<ConvTypeFilter>,
    },
    /// Teams and channels (via Graph API).
    Teams {
        #[command(subcommand)]
        subcommand: TeamsSubcommand,
    },
    /// Create config directories and default files.
    Init(InitCommand),
    /// Background daemon for token refresh and sync.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Inspect and manage configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum AuthSubcommand {
    /// Check authentication status.
    Status,
    /// Login to Microsoft Teams (opens browser, extracts tokens automatically).
    Login {
        /// Timeout in seconds for the browser login flow.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Skip automated extraction and print manual instructions.
        #[arg(long)]
        manual: bool,
    },
    /// Logout and clear stored tokens.
    Logout,
    /// Store tokens manually (fallback if automated extraction fails).
    Store {
        /// Token for api.spaces.skype.com.
        #[arg(long, env = "TMZ_SKYPE_TOKEN")]
        skype_token: Option<String>,
        /// Token for chatsvcagg.teams.microsoft.com.
        #[arg(long, env = "TMZ_CHAT_TOKEN")]
        chat_token: Option<String>,
        /// Token for graph.microsoft.com.
        #[arg(long, env = "TMZ_GRAPH_TOKEN")]
        graph_token: Option<String>,
        /// Token for presence.teams.microsoft.com.
        #[arg(long, env = "TMZ_PRESENCE_TOKEN")]
        presence_token: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Args)]
struct SyncCommand {
    /// Also sync recent messages for the top N conversations.
    #[arg(short, long, default_value_t = 30)]
    messages: usize,
    /// Number of messages per conversation to fetch.
    #[arg(short = 'n', long, default_value_t = 50)]
    per_chat: i32,
}

#[derive(Debug, Clone, Copy, Args)]
struct ChatsCommand {
    /// Max number of conversations to show.
    #[arg(short, long, default_value_t = 20)]
    limit: i64,
}

#[derive(Debug, Clone, Subcommand)]
enum TeamsSubcommand {
    /// List your teams.
    List,
    /// List channels in a team.
    Channels {
        /// Team ID.
        team_id: String,
    },
}

#[derive(Debug, Clone, Copy, Args)]
struct InitCommand {
    /// Recreate configuration even if it already exists.
    #[arg(long = "force")]
    force: bool,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum ConfigCommand {
    /// Output the effective configuration.
    Show,
    /// Print the resolved config file path.
    Path,
    /// Print all resolved paths.
    Paths,
    /// Print the JSON schema.
    Schema,
    /// Regenerate the default configuration file.
    Reset,
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum ServiceCommand {
    /// Start the background daemon.
    Start,
    /// Stop the background daemon.
    Stop,
    /// Restart the background daemon.
    Restart,
    /// Show daemon status.
    Status,
    /// Install as login service (launchd on macOS, systemd on Linux).
    Enable,
    /// Uninstall the login service.
    Disable,
    /// Run the daemon in the foreground (for debugging).
    Run,
}

// ─── Runtime ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct RuntimeContext {
    common: CommonOpts,
    paths: AppPaths,
    config: AppConfig,
}

impl RuntimeContext {
    fn new(common: CommonOpts) -> Result<Self> {
        let paths = AppPaths::discover(common.config.as_deref())?;
        let config = AppConfig::load(&paths, common.dry_run)?;
        let paths = paths.apply_overrides(&config)?;
        let ctx = Self {
            common,
            paths,
            config,
        };
        ctx.ensure_directories()?;
        Ok(ctx)
    }

    fn init_logging(&self) -> Result<()> {
        if self.common.quiet {
            log::set_max_level(LevelFilter::Off);
            return Ok(());
        }
        let mut builder =
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"));
        builder.filter_level(self.effective_log_level());

        let force_color = matches!(self.common.color, ColorOption::Always)
            || env::var_os("FORCE_COLOR").is_some();
        let disable_color = self.common.no_color
            || matches!(self.common.color, ColorOption::Never)
            || env::var_os("NO_COLOR").is_some()
            || (!force_color && !io::stderr().is_terminal());

        if disable_color {
            builder.write_style(WriteStyle::Never);
        } else if force_color {
            builder.write_style(WriteStyle::Always);
        } else {
            builder.write_style(WriteStyle::Auto);
        }

        builder.try_init().or_else(|err| {
            if self.common.verbose > 0 {
                eprintln!("logger already initialized: {err}");
            }
            Ok(())
        })
    }

    const fn effective_log_level(&self) -> LevelFilter {
        if self.common.trace {
            LevelFilter::Trace
        } else if self.common.debug {
            LevelFilter::Debug
        } else {
            match self.common.verbose {
                0 => LevelFilter::Warn,
                1 => LevelFilter::Info,
                2 => LevelFilter::Debug,
                _ => LevelFilter::Trace,
            }
        }
    }

    fn ensure_directories(&self) -> Result<()> {
        if self.common.dry_run {
            self.paths.log_dry_run();
            return Ok(());
        }
        self.paths.ensure_directories()
    }

    async fn open_cache(&self) -> Result<Cache> {
        let db_path = self.paths.data_dir.join("cache.db");
        Cache::open(&db_path).await.map_err(|e| anyhow!("{e}"))
    }

    /// Resolve a target string to a conversation ID.
    /// Checks: 1) config alias  2) exact conversation ID in cache  3) fuzzy search cache.
    async fn resolve_target(&self, cache: &Cache, target: &str) -> Result<String> {
        // 1. Config alias
        if let Some(resolved) = self.config.resolve_alias(target) {
            // The alias value might be a conversation ID or another name
            // If it looks like a conversation ID (starts with 19:), use it
            if resolved.starts_with("19:") {
                return Ok(resolved.to_string());
            }
            // Otherwise try to find the conversation by name
            let matches = cache.find_conversation(resolved).await?;
            if matches.len() == 1 {
                return Ok(matches[0].id.clone());
            }
            if matches.is_empty() {
                return Err(anyhow!("alias '{target}' resolved to '{resolved}' but no matching conversation found in cache. Run 'tmz sync' first."));
            }
            // Multiple matches - show them
            eprintln!("Alias '{target}' matched multiple conversations:");
            print_conversation_list(&matches);
            return Err(anyhow!("ambiguous alias. Use 'tmz alias {target} <exact-id>' to set a specific conversation."));
        }

        // 2. Exact conversation ID
        if target.starts_with("19:") {
            return Ok(target.to_string());
        }

        // 3. Fuzzy search
        let matches = cache.find_conversation(target).await?;
        match matches.len() {
            0 => Err(anyhow!("no conversation matching '{target}'. Run 'tmz sync' or use 'tmz find {target}'.")),
            1 => Ok(matches[0].id.clone()),
            _ => {
                eprintln!("Multiple conversations match '{target}':");
                print_conversation_list(&matches);
                Err(anyhow!("ambiguous target. Use the full conversation ID or create an alias with 'tmz alias'."))
            }
        }
    }
}

// ─── Handlers ────────────────────────────────────────────────────────

async fn handle_auth(_ctx: &RuntimeContext, cmd: AuthSubcommand) -> Result<()> {
    let auth = AuthManager::new()?;

    match cmd {
        AuthSubcommand::Status => {
            match auth.is_authenticated() {
                Ok(true) => {
                    let tokens = auth.get_tokens()?;
                    println!("Authenticated as: {}", tokens.user_principal_name);
                    println!("Tenant ID:        {}", tokens.tenant_id);

                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    let remaining = tokens.expires_at - now;
                    if remaining > 0 {
                        let mins = remaining / 60;
                        let secs = remaining % 60;
                        println!("Token expires:    {mins}m {secs}s remaining");
                    }
                }
                Ok(false) => {
                    println!("Not authenticated. Run 'tmz auth login' to authenticate.");
                }
                Err(e) => {
                    return Err(anyhow!("Error checking auth status: {e}"));
                }
            }
            Ok(())
        }
        AuthSubcommand::Login { timeout, manual } => {
            if manual {
                println!("Opening browser for manual authentication...");
                let _ = open::that_detached(AuthManager::TEAMS_URL);
                println!();
                println!("After login, extract tokens and run:");
                println!("  tmz auth store --skype-token <token> --chat-token <token> --graph-token <token> --presence-token <token>");
                return Ok(());
            }

            let tokens = auth.browser_login(Some(timeout), false).await?;
            println!("Authenticated as: {}", tokens.user_principal_name);
            println!("Tenant: {}", tokens.tenant_id);
            Ok(())
        }
        AuthSubcommand::Logout => {
            auth.logout()?;
            println!("Logged out.");
            Ok(())
        }
        AuthSubcommand::Store {
            skype_token,
            chat_token,
            graph_token,
            presence_token,
        } => {
            let skype = skype_token.ok_or_else(|| anyhow!("--skype-token is required"))?;
            let chat = chat_token.ok_or_else(|| anyhow!("--chat-token is required"))?;
            let graph = graph_token.ok_or_else(|| anyhow!("--graph-token is required"))?;
            let presence = presence_token.ok_or_else(|| anyhow!("--presence-token is required"))?;
            let tokens = auth.store_tokens(&skype, &chat, &graph, &presence)?;
            println!("Stored tokens for: {}", tokens.user_principal_name);
            Ok(())
        }
    }
}

async fn handle_sync(ctx: &RuntimeContext, cmd: SyncCommand) -> Result<()> {
    let client = TeamsClient::new()?;
    let db = ctx.open_cache().await?;

    // 1. Sync conversations
    eprint!("Syncing conversations... ");
    let data = client.list_chats().await?;
    let conversations = data["conversations"]
        .as_array()
        .ok_or_else(|| anyhow!("unexpected API response: missing conversations array"))?;

    let mut conv_count = 0u64;
    for conv in conversations {
        let cached = cache::parse_conversation(conv);
        db.upsert_conversation(&cached).await?;
        conv_count += 1;
    }
    eprintln!("{conv_count} conversations.");

    // 2. Sync messages for top N conversations (by last activity)
    if cmd.messages > 0 {
        let top_convs = db.list_conversations(cmd.messages as i64).await?;
        let total = top_convs.len();
        let mut msg_count = 0u64;

        for (i, conv) in top_convs.iter().enumerate() {
            let name = if conv.display_name.is_empty() {
                &conv.id
            } else {
                &conv.display_name
            };
            let short_name: String = name.chars().take(40).collect();
            eprint!("\rSyncing messages [{}/{}] {short_name:<40}", i + 1, total);

            match client
                .get_chat_messages(&conv.id, Some(cmd.per_chat))
                .await
            {
                Ok(msg_data) => {
                    if let Some(messages) = msg_data["messages"].as_array() {
                        for msg in messages {
                            if let Some(cached) = cache::parse_message(msg, &conv.id) {
                                db.upsert_message(&cached).await?;
                                msg_count += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("failed to sync messages for {}: {e}", conv.id);
                }
            }
        }
        eprintln!("\r{msg_count} messages across {total} conversations.{:>40}", "");
    }

    let stats = db.stats().await?;
    println!(
        "Cache: {} conversations, {} messages.",
        stats.conversations, stats.messages
    );

    Ok(())
}

async fn handle_chats(ctx: &RuntimeContext, cmd: ChatsCommand) -> Result<()> {
    let db = ctx.open_cache().await?;
    let convs = db.list_conversations(cmd.limit).await?;

    if convs.is_empty() {
        println!("No conversations cached. Run 'tmz sync' first.");
        return Ok(());
    }

    if ctx.common.json {
        let json: Vec<serde_json::Value> = convs
            .iter()
            .filter_map(|c| serde_json::from_str(&c.raw_json).ok())
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    print_conversation_list(&convs);
    Ok(())
}

async fn handle_msg(
    ctx: &RuntimeContext,
    target: String,
    message: Option<String>,
    file: Option<PathBuf>,
    limit: i64,
    no_images: bool,
) -> Result<()> {
    let db = ctx.open_cache().await?;
    let conv_id = ctx.resolve_target(&db, &target).await?;

    // Send file if --file is specified
    if let Some(ref file_path) = file {
        if !file_path.exists() {
            return Err(anyhow!("file not found: {}", file_path.display()));
        }
        let client = TeamsClient::new()?;
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        eprint!("Uploading {file_name}... ");
        client.send_file(&conv_id, file_path).await?;
        eprintln!("done.");

        // Also send text message if provided
        if let Some(ref msg_text) = message {
            client.send_message(&conv_id, msg_text).await?;
        }
        println!("Sent.");
        return Ok(());
    }

    if let Some(msg_text) = message {
        // Send a text message
        let client = TeamsClient::new()?;
        client.send_message(&conv_id, &msg_text).await?;
        println!("Sent.");
        return Ok(());
    }

    // Show recent messages (prefer cache, fall back to API)
    let messages = db.get_messages(&conv_id, limit).await?;

    if messages.is_empty() {
        // Try live fetch
        eprintln!("No cached messages. Fetching from API...");
        let client = TeamsClient::new()?;
        let limit_i32 = i32::try_from(limit).unwrap_or(20);
        let data = client.get_chat_messages(&conv_id, Some(limit_i32)).await?;
        if ctx.common.json {
            println!("{}", serde_json::to_string_pretty(&data)?);
            return Ok(());
        }
        if let Some(msgs) = data["messages"].as_array() {
            let parsed: Vec<_> = msgs
                .iter()
                .filter_map(|m| cache::parse_message(m, &conv_id))
                .collect();
            let groups = group_messages(&parsed);
            let mut prev_g: Option<&MessageGroup<'_>> = None;
            for g in &groups {
                print_bubble(g, prev_g);
                prev_g = Some(g);
            }
        }
        return Ok(());
    }

    if ctx.common.json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
        return Ok(());
    }

    // Print header
    let convs = db.find_conversation(&conv_id).await?;
    if let Some(conv) = convs.first() {
        println!("\x1b[1m{}\x1b[0m", conv.display_name);
        println!();
    }

    let show_images = !no_images && tmz_core::kitty::is_supported();
    let client = if show_images {
        TeamsClient::new().ok()
    } else {
        None
    };

    // Group consecutive messages from the same sender into bubbles
    let groups = group_messages(&messages);
    let mut prev_group: Option<&MessageGroup<'_>> = None;

    for group in &groups {
        print_bubble(group, prev_group);

        // Render inline images via Kitty protocol after the bubble
        if show_images
            && let Some(ref client) = client
        {
            for msg in &group.messages {
                let urls = tmz_core::kitty::extract_image_urls(&msg.content_html);
                for url in &urls {
                    match client.download_image(url).await {
                        Ok(data) => {
                            if let Err(e) = tmz_core::kitty::display_image(&data) {
                                debug!("kitty image render failed: {e}");
                            }
                        }
                        Err(e) => debug!("image download failed: {e}"),
                    }
                }
            }
        }

        prev_group = Some(group);
    }

    Ok(())
}

async fn handle_search(
    ctx: &RuntimeContext,
    query: &str,
    chat: Option<&str>,
    limit: i64,
) -> Result<()> {
    let db = ctx.open_cache().await?;

    let (results, scope_name) = if let Some(target) = chat {
        let conv_id = ctx.resolve_target(&db, target).await?;
        let convs = db.find_conversation(&conv_id).await?;
        let name = convs
            .first()
            .map_or_else(|| conv_id.clone(), |c| c.display_name.clone());
        let res = db.search_in_conversation(query, &conv_id, limit).await?;
        (res, Some(name))
    } else {
        let res = db.search(query, limit).await?;
        (res, None)
    };

    if results.is_empty() {
        if let Some(ref name) = scope_name {
            println!("No results for '{query}' in {name}.");
        } else {
            println!("No results for '{query}'.");
        }
        return Ok(());
    }

    if ctx.common.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    // Header
    if let Some(ref name) = scope_name {
        println!(
            "\x1b[1m{}\x1b[0m result(s) for '\x1b[1m{query}\x1b[0m' in \x1b[1m{name}\x1b[0m",
            results.len()
        );
    } else {
        println!(
            "\x1b[1m{}\x1b[0m result(s) for '\x1b[1m{query}\x1b[0m'",
            results.len()
        );
    }

    let w = term_width();
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    let mut prev_date: Option<String> = None;
    for r in &results {
        let date = extract_date(&r.message.compose_time);

        // Date separator
        if prev_date.as_deref() != Some(&date) {
            let label = format_date_label(&date);
            let total_pad = w.saturating_sub(label.len() + 4);
            let left = total_pad / 2;
            let right = total_pad - left;
            println!(
                "\x1b[2m{:\u{2500}<left$} {label} {:\u{2500}<right$}\x1b[0m",
                "", ""
            );
            prev_date = Some(date);
        }

        let time = format_time_short(&r.message.compose_time);
        let name = if r.message.from_display_name.is_empty() {
            "(system)"
        } else {
            &r.message.from_display_name
        };
        let conv = if scope_name.is_some() || r.conversation_name.is_empty() {
            String::new()
        } else {
            format!(" \x1b[2min {}\x1b[0m", r.conversation_name)
        };

        let bar_color = if r.message.is_from_me { "36" } else { "33" };
        let name_color = if r.message.is_from_me {
            "1;36"
        } else {
            "1;33"
        };

        // Header line
        let name_vis = visible_len(name) + visible_len(&conv);
        let time_vis = visible_len(&time);
        let content_w = w.saturating_sub(6);
        let gap = content_w.saturating_sub(name_vis + time_vis);
        println!(
            "  \x1b[{bar_color}m\u{2502}\x1b[0m \x1b[{name_color}m{name}\x1b[0m{conv}{:gap$}\x1b[2m{time}\x1b[0m",
            ""
        );

        // Content with highlighted matches
        let content = r.message.content.trim();
        let content_w_inner = w.saturating_sub(6);
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let shortened = shorten_urls(trimmed, 50);
            let highlighted = highlight_matches(&shortened, &query_words);
            let wrapped = wrap_lines(&[highlighted], content_w_inner);
            for wl in &wrapped {
                println!("  \x1b[{bar_color}m\u{2502}\x1b[0m {wl}");
            }
        }
        println!();
    }

    Ok(())
}

async fn handle_find(
    ctx: &RuntimeContext,
    query: &str,
    conv_type: Option<ConvTypeFilter>,
) -> Result<()> {
    let db = ctx.open_cache().await?;
    let all_matches = db.find_conversation(query).await?;

    let matches: Vec<_> = if let Some(filter) = conv_type {
        all_matches
            .into_iter()
            .filter(|c| filter.matches(&c.product_type))
            .collect()
    } else {
        all_matches
    };

    if matches.is_empty() {
        let hint = conv_type.map_or(String::new(), |f| format!(" (filter: {f:?})"));
        println!("No conversations matching '{query}'{hint}. Run 'tmz sync' first.");
        return Ok(());
    }

    if ctx.common.json {
        let json: Vec<serde_json::Value> = matches
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "display_name": c.display_name,
                    "product_type": c.product_type,
                    "last_activity": c.last_activity,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    println!("{} conversation(s) matching '{query}':\n", matches.len());
    for c in &matches {
        let kind = format_chat_type(&c.product_type);
        let time = format_time(&c.last_activity);
        println!("  {kind:>9}  {}", c.display_name);
        println!("           {time}");
        println!("           ID: {}", c.id);
        println!();
    }

    println!("Create an alias:  tmz alias <name> <id>");

    Ok(())
}

async fn handle_alias(
    ctx: &RuntimeContext,
    name: &str,
    target: Option<String>,
    conv_type: Option<ConvTypeFilter>,
) -> Result<()> {
    let db = ctx.open_cache().await?;

    let conv_id = if let Some(ref t) = target {
        if t.starts_with("19:") {
            // Direct conversation ID - no filtering needed
            t.clone()
        } else {
            // Search and optionally filter by type
            let all_matches = db.find_conversation(t).await?;
            let matches: Vec<_> = if let Some(filter) = conv_type {
                all_matches
                    .into_iter()
                    .filter(|c| filter.matches(&c.product_type))
                    .collect()
            } else {
                all_matches
            };

            match matches.len() {
                0 => {
                    let hint = conv_type.map_or(String::new(), |f| format!(" (filter: {f:?})"));
                    return Err(anyhow!("no conversation matching '{t}'{hint}."));
                }
                1 => matches[0].id.clone(),
                _ => {
                    eprintln!("Multiple matches for '{t}':");
                    print_conversation_list(&matches);
                    return Err(anyhow!(
                        "ambiguous. Use -t to filter (1:1, group, channel, meeting) or pass an exact ID."
                    ));
                }
            }
        }
    } else {
        return Err(anyhow!(
            "usage: tmz alias <name> <conversation-id-or-search-term>"
        ));
    };

    // Look up display name for confirmation
    let convs = db.find_conversation(&conv_id).await?;
    let display = convs
        .first()
        .map_or("(unknown)", |c| c.display_name.as_str());

    AppConfig::add_alias(&ctx.paths.config_file, name, &conv_id)?;
    println!("Alias '{name}' -> {display}");
    println!("  ID: {conv_id}");
    println!("  Written to: {}", ctx.paths.config_file.display());

    Ok(())
}

async fn handle_teams(ctx: &RuntimeContext, cmd: TeamsSubcommand) -> Result<()> {
    let client = TeamsClient::new()?;

    match cmd {
        TeamsSubcommand::List => {
            let teams = client.list_teams().await?;

            if ctx.common.json {
                println!("{}", serde_json::to_string_pretty(&teams)?);
                return Ok(());
            }

            for team in &teams {
                let name = team["displayName"].as_str().unwrap_or("?");
                let desc = team["description"].as_str().unwrap_or("");
                let id = team["id"].as_str().unwrap_or("?");
                println!("  {name}");
                if !desc.is_empty() {
                    println!("    {}", truncate(desc, 80));
                }
                println!("    ID: {id}");
                println!();
            }
            Ok(())
        }
        TeamsSubcommand::Channels { team_id } => {
            let channels = client.list_channels(&team_id).await?;

            if ctx.common.json {
                println!("{}", serde_json::to_string_pretty(&channels)?);
                return Ok(());
            }

            for ch in &channels {
                let name = ch["displayName"].as_str().unwrap_or("?");
                let id = ch["id"].as_str().unwrap_or("?");
                println!("  {name}");
                println!("    ID: {id}");
            }
            Ok(())
        }
    }
}

async fn handle_service(ctx: &RuntimeContext, cmd: ServiceCommand) -> Result<()> {
    use tmz_core::daemon;

    match cmd {
        ServiceCommand::Start => service_start(),
        ServiceCommand::Stop => {
            daemon::stop_daemon()?;
            println!("Daemon stopped.");
            Ok(())
        }
        ServiceCommand::Restart => {
            if daemon::is_running()? {
                daemon::stop_daemon()?;
                println!("Daemon stopped.");
            }
            service_start()
        }
        ServiceCommand::Status => service_status(ctx),
        ServiceCommand::Enable => service_enable(),
        ServiceCommand::Disable => service_disable(),
        ServiceCommand::Run => daemon::run_daemon().await.map_err(|e| anyhow!("{e}")),
    }
}

fn service_start() -> Result<()> {
    use tmz_core::daemon;

    if daemon::is_running()? {
        println!(
            "Daemon is already running (pid={}).",
            daemon::read_pid()?.unwrap_or(0)
        );
        return Ok(());
    }

    let exe =
        std::env::current_exe().map_err(|e| anyhow!("cannot determine executable path: {e}"))?;
    let log_path = daemon::log_file_path()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let child = std::process::Command::new(exe)
        .args(["service", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    println!("Daemon started (pid={}).", child.id());
    println!("Log: {}", log_path.display());
    Ok(())
}

fn service_status(_ctx: &RuntimeContext) -> Result<()> {
    use tmz_core::daemon;

    if daemon::is_running()? {
        let pid = daemon::read_pid()?.unwrap_or(0);
        let log_path = daemon::log_file_path()?;
        println!("running  (pid={pid})");
        println!("log:     {}", log_path.display());

        let auth = AuthManager::new()?;
        match auth.get_tokens() {
            Ok(tokens) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let remaining = tokens.expires_at - now;
                if remaining > 0 {
                    let mins = remaining / 60;
                    let secs = remaining % 60;
                    println!("tokens:  valid ({mins}m {secs}s remaining)");
                } else {
                    println!("tokens:  expired");
                }
            }
            Err(_) => println!("tokens:  none"),
        }
    } else {
        println!("stopped");
    }
    Ok(())
}

fn service_enable() -> Result<()> {
    use tmz_core::daemon;

    let exe =
        std::env::current_exe().map_err(|e| anyhow!("cannot determine executable path: {e}"))?;
    let exe_str = exe.to_string_lossy();
    let home =
        dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;

    if cfg!(target_os = "macos") {
        let plist_dir = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&plist_dir)?;
        let plist_path = plist_dir.join("de.byteowlz.tmz.plist");
        std::fs::write(&plist_path, daemon::launchd_plist(&exe_str))?;

        let status = std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&plist_path)
            .status()?;
        if status.success() {
            println!("Service enabled (launchd).");
            println!("Plist: {}", plist_path.display());
        } else {
            return Err(anyhow!("launchctl load failed"));
        }
    } else {
        let unit_dir = home.join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir)?;
        let unit_path = unit_dir.join("tmz.service");
        std::fs::write(&unit_path, daemon::systemd_unit(&exe_str))?;

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "tmz.service"])
            .status()?;
        if status.success() {
            println!("Service enabled (systemd).");
            println!("Unit: {}", unit_path.display());
        } else {
            return Err(anyhow!("systemctl enable failed"));
        }
    }
    Ok(())
}

fn service_disable() -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;

    if cfg!(target_os = "macos") {
        let plist_path = home.join("Library/LaunchAgents/de.byteowlz.tmz.plist");
        if plist_path.exists() {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&plist_path)
                .status();
            std::fs::remove_file(&plist_path)?;
            println!("Service disabled (launchd).");
        } else {
            println!("Service not installed.");
        }
    } else {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "tmz.service"])
            .status();
        let unit_path = home.join(".config/systemd/user/tmz.service");
        if unit_path.exists() {
            std::fs::remove_file(&unit_path)?;
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
        }
        println!("Service disabled (systemd).");
    }
    Ok(())
}

fn handle_init(ctx: &RuntimeContext, cmd: InitCommand) -> Result<()> {
    if ctx.paths.config_file.exists() && !(cmd.force || ctx.common.assume_yes) {
        return Err(anyhow!(
            "config already exists at {} (use --force to overwrite)",
            ctx.paths.config_file.display()
        ));
    }
    if ctx.common.dry_run {
        log::info!(
            "dry-run: would write default config to {}",
            ctx.paths.config_file.display()
        );
        return Ok(());
    }
    write_default_config(&ctx.paths.config_file)
}

fn handle_config(ctx: &RuntimeContext, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Show => {
            if ctx.common.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&ctx.config)
                        .context("serializing config to JSON")?
                );
            } else {
                println!("{:#?}", ctx.config);
            }
            Ok(())
        }
        ConfigCommand::Path => {
            println!("{}", ctx.paths.config_file.display());
            Ok(())
        }
        ConfigCommand::Paths => {
            let cache_dir = default_cache_dir()?;
            if ctx.common.json {
                let paths = serde_json::json!({
                    "config": ctx.paths.config_file,
                    "data": ctx.paths.data_dir,
                    "state": ctx.paths.state_dir,
                    "cache": cache_dir,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&paths).context("serializing paths to JSON")?
                );
            } else {
                println!("config: {}", ctx.paths.config_file.display());
                println!("data:   {}", ctx.paths.data_dir.display());
                println!("state:  {}", ctx.paths.state_dir.display());
                println!("cache:  {}", cache_dir.display());
            }
            Ok(())
        }
        ConfigCommand::Schema => {
            println!("{}", include_str!("../../../examples/config.schema.json"));
            Ok(())
        }
        ConfigCommand::Reset => {
            if ctx.common.dry_run {
                log::info!(
                    "dry-run: would reset config at {}",
                    ctx.paths.config_file.display()
                );
                return Ok(());
            }
            write_default_config(&ctx.paths.config_file)
        }
    }
}

fn handle_completions(shell: Shell) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, APP_NAME, &mut io::stdout());
}

// ─── Formatting helpers ──────────────────────────────────────────────

fn print_conversation_list(convs: &[tmz_core::CachedConversation]) {
    for c in convs {
        let kind = format_chat_type(&c.product_type);
        let time = format_time(&c.last_activity);
        let name = if c.display_name.is_empty() {
            "(unnamed)"
        } else {
            &c.display_name
        };

        let preview = if c.last_message_from.is_empty() {
            truncate(&c.last_message_preview, 60)
        } else {
            let full = format!("{}: {}", c.last_message_from, c.last_message_preview);
            truncate(&full, 60)
        };

        println!("  {kind:>9}  {name}");
        println!("           {time}  {preview}");
        println!("           {}", dim(&c.id));
        println!();
    }
}

// ── Message rendering ────────────────────────────────────────────────
//
// Clean chat layout inspired by pi / opencode:
//   - Colored left-border bar (|) per sender
//   - Sender name bold + colored, time dimmed on same line
//   - Content indented past the bar
//   - Date separators between days
//   - URLs truncated to fit terminal width
//   - Compact vertical spacing

/// Terminal width, clamped to a reasonable range.
fn term_width() -> usize {
    terminal_size::terminal_size()
        .map_or(80, |(w, _)| usize::from(w.0))
        .clamp(40, 200)
}

/// A group of consecutive messages from the same sender.
struct MessageGroup<'a> {
    sender: &'a str,
    is_from_me: bool,
    messages: Vec<&'a tmz_core::CachedMessage>,
}

impl MessageGroup<'_> {
    fn first_date(&self) -> String {
        self.messages
            .first()
            .map(|m| extract_date(&m.compose_time))
            .unwrap_or_default()
    }

    fn last_time(&self) -> String {
        self.messages
            .last()
            .map(|m| format_time_short(&m.compose_time))
            .unwrap_or_default()
    }
}

/// Group consecutive messages from the same sender on the same day.
fn group_messages(messages: &[tmz_core::CachedMessage]) -> Vec<MessageGroup<'_>> {
    let mut groups: Vec<MessageGroup<'_>> = Vec::new();

    for msg in messages {
        let same_sender = groups.last().is_some_and(|g| {
            g.sender == msg.from_display_name
                && g.first_date() == extract_date(&msg.compose_time)
        });

        if same_sender {
            if let Some(last) = groups.last_mut() {
                last.messages.push(msg);
            }
        } else {
            groups.push(MessageGroup {
                sender: if msg.from_display_name.is_empty() {
                    "(system)"
                } else {
                    &msg.from_display_name
                },
                is_from_me: msg.is_from_me,
                messages: vec![msg],
            });
        }
    }

    groups
}

/// Print a centered date separator when the day changes.
fn maybe_print_date_separator(date: &str, prev_date: Option<&str>) {
    if prev_date == Some(date) {
        return;
    }
    let label = format_date_label(date);
    let w = term_width();
    let total_pad = w.saturating_sub(label.len() + 4);
    let left = total_pad / 2;
    let right = total_pad - left;
    if prev_date.is_some() {
        println!();
    }
    // Thin line with centered date label
    println!(
        "\x1b[2m{:\u{2500}<left$} {label} {:\u{2500}<right$}\x1b[0m",
        "", ""
    );
}

/// Render a message group with a colored left border.
///
/// Layout:
/// ```text
///   \u{2502} Sender Name                              14:35
///   \u{2502} Message content here that wraps nicely
///   \u{2502} across multiple lines if needed
/// ```
fn print_bubble(group: &MessageGroup<'_>, prev: Option<&MessageGroup<'_>>) {
    let prev_date = prev.map(MessageGroup::first_date);
    maybe_print_date_separator(&group.first_date(), prev_date.as_deref());

    // Gather content lines
    let mut lines: Vec<String> = Vec::new();
    for msg in &group.messages {
        let content = msg.content.trim();
        let has_images = !tmz_core::kitty::extract_image_urls(&msg.content_html).is_empty();

        if !content.is_empty() {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    lines.push(shorten_urls(trimmed, 50));
                }
            }
        } else if has_images {
            lines.push("\x1b[2m[image]\x1b[0m".to_string());
        }
    }

    if lines.is_empty() {
        return;
    }

    let time = group.last_time();
    let name = group.sender;
    let w = term_width();
    let content_w = w.saturating_sub(6); // "  | " prefix + margin

    // Bar color: cyan for self, yellow for others, dim for system
    let bar_color = if name == "(system)" {
        "2"
    } else if group.is_from_me {
        "36"
    } else {
        "33"
    };
    let name_color = if group.is_from_me { "1;36" } else { "1;33" };

    // Blank line between groups (not after date separator)
    if prev.is_some() && prev_date.as_deref() == Some(&group.first_date()) {
        println!();
    }

    // Header: bar + name + time right-aligned
    let name_vis = visible_len(name);
    let time_vis = visible_len(&time);
    let gap = content_w.saturating_sub(name_vis + time_vis);
    println!(
        "  \x1b[{bar_color}m\u{2502}\x1b[0m \x1b[{name_color}m{name}\x1b[0m{:gap$}\x1b[2m{time}\x1b[0m",
        ""
    );

    // Content lines with word-wrap
    let wrapped = wrap_lines(&lines, content_w);
    for line in &wrapped {
        println!("  \x1b[{bar_color}m\u{2502}\x1b[0m {line}");
    }
}

/// Highlight search query words in text using bold + underline.
fn highlight_matches(text: &str, query_words: &[&str]) -> String {
    if query_words.is_empty() {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len() * 2);
    let lower = text.to_lowercase();
    let mut pos = 0;

    while pos < text.len() {
        let mut best_match: Option<(usize, usize)> = None; // (start, end)

        for word in query_words {
            if let Some(found) = lower[pos..].find(word) {
                let abs_start = pos + found;
                let abs_end = abs_start + word.len();
                if best_match.is_none_or(|(s, _)| abs_start < s) {
                    best_match = Some((abs_start, abs_end));
                }
            }
        }

        if let Some((start, end)) = best_match {
            // Text before match
            result.push_str(&text[pos..start]);
            // Highlighted match (bold + magenta)
            result.push_str("\x1b[1;35m");
            result.push_str(&text[start..end]);
            result.push_str("\x1b[0m");
            pos = end;
        } else {
            result.push_str(&text[pos..]);
            break;
        }
    }

    result
}

/// Shorten URLs in text to a maximum display length.
///
/// `https://www.linkedin.com/posts/very-long-path?utm_source=...` becomes
/// `linkedin.com/.../very-long-path...`
fn shorten_urls(text: &str, max_url_len: usize) -> String {
    use std::fmt::Write;

    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("http") {
        result.push_str(&remaining[..start]);

        let url_str = &remaining[start..];
        let end = url_str
            .find(|c: char| c.is_whitespace())
            .unwrap_or(url_str.len());
        let url = &url_str[..end];

        if url.len() > max_url_len {
            let shortened = shorten_single_url(url, max_url_len);
            let _ = write!(result, "\x1b[2;4m{shortened}\x1b[0m");
        } else {
            let _ = write!(result, "\x1b[2;4m{url}\x1b[0m");
        }

        remaining = &url_str[end..];
    }
    result.push_str(remaining);
    result
}

/// Shorten a single URL to fit within `max_len` characters.
fn shorten_single_url(url: &str, max_len: usize) -> String {
    // Strip protocol
    let without_proto = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Strip www.
    let clean = without_proto.strip_prefix("www.").unwrap_or(without_proto);

    if clean.len() <= max_len {
        return clean.to_string();
    }

    // Get domain
    let slash_pos = clean.find('/').unwrap_or(clean.len());
    let domain = &clean[..slash_pos];

    // Strip query params for display
    let path = &clean[slash_pos..];
    let path_no_query = path.split('?').next().unwrap_or(path);
    let path_no_query = path_no_query.split('#').next().unwrap_or(path_no_query);

    let candidate = format!("{domain}{path_no_query}");
    if candidate.len() <= max_len {
        return candidate;
    }

    // Truncate path
    let budget = max_len.saturating_sub(domain.len() + 4); // domain + /...
    let path_truncated: String = path_no_query.chars().take(budget).collect();
    format!("{domain}{path_truncated}...")
}

/// Wrap lines to fit a maximum width, handling long words by hard-breaking.
fn wrap_lines(lines: &[String], max_width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in lines {
        if visible_len(line) <= max_width {
            result.push(line.clone());
        } else {
            let mut current = String::new();
            let mut current_len = 0;
            for word in line.split_whitespace() {
                let wlen = visible_len(word);
                if current.is_empty() {
                    // Single word longer than max -> hard break
                    if wlen > max_width {
                        let chars = word.chars();
                        let mut chunk = String::new();
                        let mut clen = 0;
                        for ch in chars {
                            let ch_w =
                                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                            if clen + ch_w > max_width && !chunk.is_empty() {
                                result.push(chunk);
                                chunk = String::new();
                                clen = 0;
                            }
                            chunk.push(ch);
                            clen += ch_w;
                        }
                        current = chunk;
                        current_len = clen;
                    } else {
                        current = word.to_string();
                        current_len = wlen;
                    }
                } else if current_len + 1 + wlen <= max_width {
                    current.push(' ');
                    current.push_str(word);
                    current_len += 1 + wlen;
                } else {
                    result.push(current);
                    current = word.to_string();
                    current_len = wlen;
                }
            }
            if !current.is_empty() {
                result.push(current);
            }
        }
    }
    result
}

/// Visible length of a string (ignoring ANSI escape sequences).
fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            len += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    len
}

// ── Date/time formatting ─────────────────────────────────────────────

/// Extract the date portion "2026-02-17" from an ISO timestamp.
fn extract_date(iso: &str) -> String {
    iso.get(..10).unwrap_or(iso).to_string()
}

/// Format a date for separator lines: "February 17, 2026".
fn format_date_label(date: &str) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return date.to_string();
    }
    let month = match parts[1] {
        "01" => "January",
        "02" => "February",
        "03" => "March",
        "04" => "April",
        "05" => "May",
        "06" => "June",
        "07" => "July",
        "08" => "August",
        "09" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
        _ => return date.to_string(),
    };
    let day = parts[2].trim_start_matches('0');
    format!("{month} {day}, {}", parts[0])
}

/// Format time as "HH:MM" for message timestamps.
fn format_time_short(iso: &str) -> String {
    if iso.len() >= 16 {
        iso[11..16].to_string()
    } else {
        iso.to_string()
    }
}

/// Format full time for search results: "Feb 17 13:43".
fn format_time(iso: &str) -> String {
    if iso.len() >= 16 {
        let date_part = &iso[..10];
        let time_part = &iso[11..16];
        if let Some(month_day) = parse_month_day(date_part) {
            return format!("{month_day} {time_part}");
        }
        return format!("{date_part} {time_part}");
    }
    iso.to_string()
}

fn parse_month_day(date: &str) -> Option<String> {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let month = match parts[1] {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return None,
    };
    let day = parts[2].trim_start_matches('0');
    Some(format!("{month} {day:>2}"))
}

fn format_chat_type(product_type: &str) -> &str {
    match product_type {
        "OneToOneChat" => "[1:1]",
        "GroupChat" => "[group]",
        "TeamsStandardChannel" | "TeamsPrivateChannel" | "TeamsSharedChannel" => "[channel]",
        "MeetingChat" => "[meeting]",
        _ => "[chat]",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{truncated}...")
    }
}

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}
