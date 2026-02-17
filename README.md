# tmz - Microsoft Teams CLI and TUI

A command-line interface and terminal user interface for Microsoft Teams, built by reverse-engineering the browser client.

## Features

- **Browser-based authentication** - Extracts tokens from the Teams web client
- **Secure credential storage** - Stores tokens in secure storage (age-encrypted)
- **Chat operations** - List chats, view messages, send messages
- **Teams & channels** - Browse teams and channel conversations
- **TUI interface** - Interactive terminal interface (ratatui)
- **MCP server** - Model Context Protocol for AI integration

## Quick Start

```bash
# Check authentication status
tmz auth status

# Login (opens browser)
tmz auth login

# Or manually store tokens extracted from browser
tmz auth store \
  --skype-token "eyJ0eXAi..." \
  --chat-token "eyJ0eXAi..." \
  --graph-token "eyJ0eXAi..." \
  --presence-token "eyJ0eXAi..."
```

## Authentication

Since this tool doesn't use the official Microsoft Graph API application registration, it authenticates by:

1. Opening the Teams web client in a browser
2. Having the user complete login in the browser
3. Extracting authentication tokens from the browser's localStorage
4. Storing tokens securely in the secure storage

### Token Extraction Methods

**Manual extraction:**
1. Open https://teams.microsoft.com in your browser
2. Login with your credentials
3. Open DevTools (F12) → Application/Storage → Local Storage
4. Find keys containing `accesstoken` and `login.windows.net`
5. Copy the `secret` field for these APIs:
   - `api.spaces.skype.com` - Skype/Chat API
   - `chatsvcagg.teams.microsoft.com` - Chat aggregation API
   - `graph.microsoft.com` - Microsoft Graph API
   - `presence.teams.microsoft.com` - Presence API

**Automated extraction (requires Playwright MCP):**
```bash
# Using browser automation
browser-start.js
browser-nav.js https://teams.microsoft.com/v2
# ... extract tokens programmatically
tmz auth store --skype-token ... --chat-token ... --graph-token ... --presence-token ...
```

## Commands

### Authentication
```bash
tmz auth status              # Check if authenticated
tmz auth login               # Login via browser
tmz auth logout              # Clear stored tokens
tmz auth store [options]     # Store tokens manually
```

### Chats
```bash
tmz chat list               # List all chats
tmz chat messages <chat-id> # Get messages from a chat
tmz chat show <chat-id>     # Show chat details
```

### Teams
```bash
tmz teams list              # List joined teams
tmz teams channels <team-id> # List channels in a team
tmz teams messages <team-id> <channel-id> # Get channel messages
```

### Messages
```bash
tmz message send <chat-id> "Hello World"  # Send a message
```

## Architecture

### Core Components

- **`tmz-core`** - Shared library with authentication, API client, and data models
- **`tmz-cli`** - Command-line interface
- **`tmz-tui`** - Terminal user interface (ratatui-based)
- **`tmz-api`** - HTTP API server (axum-based)
- **`tmz-mcp`** - Model Context Protocol server

### Authentication Flow

```
User → Browser (Teams) → Extract Tokens → vault Vault → API Client
```

Tokens are stored in `~/.local/share/vault.json` (age-encrypted). The vault must be unlocked with `vault unlock` before use.

### API Endpoints

The client uses these Microsoft endpoints:

- `https://graph.microsoft.com/v1.0` - Teams, chats, messages (via Graph)
- `https://api.spaces.skype.com` - Skype messaging
- `https://chatsvcagg.teams.microsoft.com` - Chat service aggregation
- `https://presence.teams.microsoft.com` - User presence

## Security

- Tokens are stored encrypted using age (scrypt KDF + ChaCha20-Poly1305)
- Vault passphrase never stored on disk
- Session-based vault unlocking (30 min timeout)
- Tokens automatically refreshed on expiry

## Configuration

Configuration files use `$XDG_CONFIG_HOME/tmz/config.toml` (default: `~/.config/tmz/config.toml`).

```toml
"$schema" = "https://raw.githubusercontent.com/byteowlz/schemas/refs/heads/main/tmz/tmz.config.schema.json"

[logging]
level = "info"

[runtime]
timeout = 60
```

## Development

```bash
# Build everything
cargo build --all

# Run CLI
cargo run -p tmz-cli -- auth status

# Run TUI
cargo run -p tmz-tui

# Run API server
cargo run -p tmz-api -- --port 3000

# Run tests
cargo test --all
```

## License

MIT

## Disclaimer

This tool is not affiliated with Microsoft. It uses undocumented APIs that may change without notice. Use at your own risk.
