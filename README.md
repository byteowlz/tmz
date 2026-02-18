# tmz

A command-line interface for Microsoft Teams.

Human-readable output by default. `--json` for machines. All reads from local SQLite cache -- sub-15ms response times.

## Install

```bash
just install-all
```

This installs the `tmz` binary, the Playwright auth script, and Chromium.

Prerequisites: Rust toolchain, Node.js.

## Quick Start

```bash
# Login (opens Chromium, extracts tokens automatically)
tmz auth login

# Sync conversations and recent messages to local cache
tmz sync

# Find someone and create a shortcut
tmz find "Schmidt" -t 1:1
tmz alias alex "Schmidt" -t 1:1

# Read messages
tmz msg alex

# Send a message
tmz msg alex "hey, got a minute?"

# Send a file
tmz msg alex -f ./report.pdf

# Search across all chats
tmz search "quarterly report"

# Search within a specific chat
tmz search "budget" -c alex

# Background daemon (token refresh + periodic sync)
tmz service start
```

## Commands

### Authentication

```bash
tmz auth login               # Automated browser login (Playwright)
tmz auth login --manual      # Manual token extraction instructions
tmz auth status              # Check token status and expiry
tmz auth logout              # Clear stored tokens
```

Tokens are stored as plain JSON at `$XDG_STATE_HOME/tmz/tokens.json` with `0600` permissions. They are short-lived JWTs (~1 hour) that the daemon refreshes automatically via headless browser.

### Messaging

```bash
tmz msg <target>                  # Show recent messages (default: 20)
tmz msg <target> -n 50            # Show last 50 messages
tmz msg <target> "hello"          # Send a text message
tmz msg <target> -f ./file.pdf    # Send a file
tmz msg <target> -f ./img.png "caption here"  # File with text
tmz msg <target> --no-images      # Skip inline image rendering
```

`<target>` is resolved in order: config alias, exact conversation ID, fuzzy cache search.

Messages are displayed with colored left-border bars per sender, grouped consecutive messages, date separators, URL shortening, and word wrapping. Inline images render via Kitty graphics protocol in supported terminals (Kitty, Ghostty, WezTerm).

### Sync and Cache

```bash
tmz sync                     # Sync all conversations + messages for top 30 chats
tmz sync -m 50 -n 100        # Top 50 chats, 100 messages each
tmz chats                    # List cached conversations
tmz chats --json             # Machine-readable output
```

### Search

```bash
tmz search "budget"               # Full-text search across all cached messages
tmz search "budget" -c alex     # Search within a specific chat
tmz search "sprint" -c "GenAI"    # Fuzzy chat name matching for -c
tmz search "report" -l 50         # Limit results
```

Search uses SQLite FTS5. Results show highlighted matches, date separators, conversation context, and URL shortening.

### Find Conversations

```bash
tmz find "Schmidt"               # Find conversations by name/member/ID
tmz find "Schmidt" -t 1:1        # Filter: only 1:1 direct messages
tmz find "standup" -t meeting    # Filter: only meeting threads
tmz find "general" -t channel    # Filter: only team channels
tmz find "project" -t group      # Filter: only group chats
```

### Aliases

Aliases map short names to conversation IDs in `config.toml`.

```bash
tmz alias alex "Schmidt" -t 1:1   # Create alias (auto-finds the 1:1 chat)
tmz alias team "Project Alpha" -t group   # Alias a group chat
tmz alias alex                          # Show what an alias resolves to
```

Type filter values: `1:1` (aliases: `dm`, `direct`), `group` (`grp`), `channel` (`chan`), `meeting` (`meet`).

### Background Daemon

```bash
tmz service start            # Start background daemon
tmz service stop             # Stop daemon
tmz service restart          # Restart daemon
tmz service status           # Show daemon status + token info
tmz service run              # Run in foreground (for debugging)
tmz service enable           # Auto-start on login (launchd/systemd)
tmz service disable          # Remove auto-start
```

The daemon refreshes tokens every ~50 minutes (headless Playwright with cached SSO cookies) and syncs conversations every 5 minutes.

### Teams and Channels

```bash
tmz teams list                    # List joined teams (via Graph API)
tmz teams channels <team-id>     # List channels in a team
```

### Configuration

```bash
tmz init                     # Create config dirs and default config.toml
tmz config show              # Print effective configuration
tmz config path              # Print config file path
tmz config paths             # Print all resolved paths (config, data, state)
tmz config schema            # Print JSON schema
tmz config reset             # Regenerate default config
tmz completions <shell>      # Generate shell completions (bash, zsh, fish)
```

## Configuration

Config at `$XDG_CONFIG_HOME/tmz/config.toml` (default: `~/.config/tmz/config.toml`).

```toml
"$schema" = "https://raw.githubusercontent.com/byteowlz/schemas/refs/heads/main/tmz/tmz.config.schema.json"

profile = "default"

[logging]
level = "info"

[runtime]
timeout = 60

[people]
alex = "19:4589f0b7-..._96c052fc-...@unq.gbl.spaces"
team = "19:abc123@thread.v2"
```

Override precedence: CLI flags > environment variables > config file.

## How It Works

### Authentication

The Teams web client uses MSAL tokens stored in `localStorage`. tmz launches a Chromium instance via Playwright, lets you complete SSO login, then extracts the tokens from the browser session.

A persistent browser profile at `$XDG_STATE_HOME/tmz/browser-profile` caches SSO cookies. After the first interactive login, subsequent refreshes run headlessly (no browser window) using the cached session.

### Native Teams APIs

Microsoft Graph API tokens obtained from the Teams web client lack `Chat.Read` scope. Instead, tmz uses the same internal endpoints as the Teams web client:

1. Exchange the MSAL skype access token via `POST teams.microsoft.com/api/authsvc/v1.0/authz`
2. Receive a `skypeToken` and region-specific `chatService` URL
3. Use the Skype-based chat endpoints (`/v1/users/ME/conversations/...`) with `Authentication: skypetoken=<token>`

File uploads go through the ASM blob store at `api.asm.skype.com`. Graph API is still used where its scopes are sufficient (listing teams, channels).

### Local Cache

Conversations and messages are cached in SQLite (via sqlx) at `$XDG_DATA_HOME/tmz/cache.db`. Full-text search uses SQLite FTS5 with auto-syncing triggers. All reads are local -- no network round-trips.

### Storage Paths

| Purpose | Path |
|---|---|
| Config | `$XDG_CONFIG_HOME/tmz/config.toml` |
| Cache DB | `$XDG_DATA_HOME/tmz/cache.db` |
| Tokens | `$XDG_STATE_HOME/tmz/tokens.json` |
| Browser profile | `$XDG_STATE_HOME/tmz/browser-profile/` |
| Auth script | `$XDG_DATA_HOME/tmz/teams-auth.mjs` |
| Daemon PID | `$XDG_STATE_HOME/tmz/tmz.pid` |
| Daemon log | `$XDG_STATE_HOME/tmz/tmz.log` |

## Architecture

```
tmz-cli     Command-line interface (this binary)
tmz-core    Shared library: auth, API client, cache, config
tmz-tui     Terminal UI (ratatui) [planned]
tmz-mcp     Model Context Protocol server [planned]
tmz-api     HTTP API server (axum) [planned]
```

## Global Flags

| Flag | Description |
|---|---|
| `--json` | Machine-readable JSON output |
| `--config <path>` | Override config file |
| `-q` / `--quiet` | Suppress non-error output |
| `-v` / `-vv` | Increase verbosity |
| `--debug` / `--trace` | Debug or trace logging |
| `--no-color` | Disable ANSI colors |
| `--dry-run` | Preview without side effects |
| `-y` / `--yes` | Skip interactive prompts |

## Development

```bash
just                     # List all commands
just install-all         # Install binary + auth scripts
just build               # Debug build
just build-release       # Release build
just clippy              # Lint
just test                # Run tests
just fmt                 # Format
just check-all           # Format + lint + test
just setup-auth          # Install Playwright + Chromium for auth
just generate-config     # Regenerate example config and schema
```

## Disclaimer

This tool is not affiliated with Microsoft. It uses internal APIs that may change without notice. Use at your own risk.

## License

MIT
