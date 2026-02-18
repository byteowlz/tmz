//! tmz-tui: Terminal UI for Microsoft Teams.
//!
//! Layout:
//! ```text
//! +-- Chats ------+---- Messages ----------+-- Files ---+
//! | [search...]   |                        |            |
//! |               | Sender Name      10:30 |            |
//! | > Chat 1      | Message text here...   |            |
//! |   Chat 2      |                        |            |
//! |   Chat 3      | Another Sender   10:31 |            |
//! |               | Reply text...          |            |
//! |               |                        |            |
//! +---------------+------------------------+            |
//! | [message input...]                     |            |
//! +----------------------------------------+------------+
//! | NORMAL | Connected | 52m remaining | synced 2m ago  |
//! +----------------------------------------+------------+
//! ```

mod app;
mod event;
mod ui;

use anyhow::Result;
use clap::{Args, Parser};
use std::path::PathBuf;

fn main() -> Result<()> {
    let cli = Cli::parse();
    app::run(cli.common.config.as_ref())
}

#[derive(Debug, Parser)]
#[command(
    name = "tmz-tui",
    author,
    version,
    about = "Terminal UI for Microsoft Teams"
)]
struct Cli {
    #[command(flatten)]
    common: CommonOpts,
}

#[derive(Debug, Clone, Args)]
struct CommonOpts {
    /// Override the config file path.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}
