# tmz

A command-line interface for Microsoft Teams.

Human-readable output by default. `--json` for machines.

## Install

```bash
cargo install --path crates/tmz-cli
```

Prerequisites:

- [vault](https://github.com/byteowlz/vault) - encrypted credential vault (token storage)
- Node.js - for browser-based authentication

```bash
# Install Playwright + Chromium for auth
just setup-auth

# Initialize the secure storage (if not already done)
vault init
```

## Quick Start

```bash
# Login (opens Chromium, extracts tokens automatically)
tmz auth login

# Sync conversations and recent messages to local cache
tmz sync

# List your chats
tmz chats

# Find someone and create a shortcut
tmz find "Schmidt" -t 1:1
tmz alias alex "Schmidt" -t 1:1

# Read messages
tmz msg alex

# Send a message
tmz msg alex "hey, got a minute?"

# Send a file
tmz msg alex -f ./report.pdf

# Search across all cached messages
tmz search "quarterly report"
```

## Commands

### Authentication

```bash
tmz auth login               # Automated browser login (Playwright)
tmz auth login --manual      # Opens browser with manual token extraction instructions
tmz auth status              # Check if authenticated and token expiry
tmz auth logout              # Clear stored tokens
tmz auth store --skype-token "..." --chat-token "..."   # Manual token storage
```

### Messaging

```bash
tmz msg <target>                  # Show recent messages (default: 20)
tmz msg <target> -n 50            # Show last 50 messages
tmz msg <target> "hello"          # Send a text message
tmz msg <target> -f ./file.pdf    # Send a file
tmz msg <target> -f ./img.png "caption here"  # File with text
```

`<target>` is resolved in order: config alias, exact conversation ID, fuzzy cache search.

### Sync and Cache

```bash
tmz sync                     # Sync all conversations + messages for top 30 chats
tmz sync -m 50 -n 100        # Top 50 chats, 100 messages each
tmz chats                    # List cached conversations
tmz chats --json             # Machine-readable output
```

### Search and Discovery

```bash
tmz search "budget"              # Full-text search (FTS5) across cached messages
tmz find "Schmidt"               # Find conversations by name/member/ID
tmz find "Schmidt" -t 1:1        # Filter: only 1:1 direct messages
tmz find "standup" -t meeting    # Filter: only meeting threads
tmz find "general" -t channel    # Filter: only team channels
tmz find "project" -t group      # Filter: only group chats
```

### Aliases

Aliases are stored in the `[people]` section of `config.toml` and map short names to conversation IDs.

```bash
tmz alias alex "Schmidt" -t 1:1   # Create alias (auto-finds the 1:1 chat)
tmz alias team "Project Alpha" -t group   # Alias a group chat
tmz alias alex                          # Show what an alias resolves to
```

Type filter values: `1:1` (aliases: `dm`, `direct`), `group` (`grp`), `channel` (`chan`), `meeting` (`meet`).

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

Config lives at `$XDG_CONFIG_HOME/tmz/config.toml` (default: `~/.config/tmz/config.toml`).

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

The Teams web client uses MSAL tokens stored in `localStorage`. Since there is no public API registration for personal use, tmz launches a Chromium instance via Playwright, lets you complete SSO login, then extracts the tokens from the browser session.

Tokens are stored encrypted in the [vault](https://github.com/byteowlz/vault) vault (age encryption). The vault must be unlocked (`vault unlock`) before tmz can access tokens. A persistent browser profile at `$XDG_STATE_HOME/tmz/browser-profile` caches SSO cookies for faster re-authentication.

### Native Teams APIs

Microsoft Graph API tokens obtained from the Teams web client lack `Chat.Read` scope, making them insufficient for reading chat messages. Instead, tmz uses the same internal endpoints as the Teams web client:

1. Exchange the MSAL skype access token via `POST teams.microsoft.com/api/authsvc/v1.0/authz`
2. Receive a `skypeToken` and region-specific `chatService` URL
3. Use the Skype-based chat endpoints (`/v1/users/ME/conversations/...`) with `Authentication: skypetoken=<token>`

File uploads go through the ASM blob store at `api.asm.skype.com`.

Graph API is still used where its scopes are sufficient (listing teams, channels).

### Local Cache

Conversations and messages are cached in SQLite (via sqlx) at `$XDG_DATA_HOME/tmz/cache.db`. Full-text search uses SQLite FTS5 with auto-syncing triggers. This allows fast offline search and instant alias resolution without hitting the network.

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
