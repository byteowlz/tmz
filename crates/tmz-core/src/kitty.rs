//! Kitty terminal image protocol support.
//!
//! Renders images inline in terminals that support the Kitty graphics protocol
//! (Kitty, `WezTerm`, Ghostty, etc.) using base64-encoded escape sequences.

use base64::Engine;
use std::io::{self, Write};

/// Maximum width in terminal columns for displayed images.
const MAX_COLS: u32 = 60;

/// Check whether the terminal likely supports the Kitty graphics protocol.
///
/// Checks `$TERM` and `$TERM_PROGRAM` environment variables.
#[must_use]
pub fn is_supported() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

    term.contains("kitty")
        || term.contains("xterm-kitty")
        || term_program.contains("kitty")
        || term_program.contains("WezTerm")
        || term_program.contains("ghostty")
        || term_program.contains("Ghostty")
        || std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
}

/// Display an image inline in the terminal using the Kitty graphics protocol.
///
/// The image bytes should be PNG or JPEG. The image is scaled to fit
/// within `MAX_COLS` terminal columns.
///
/// # Errors
///
/// Returns an error if stdout cannot be written to.
pub fn display_image(data: &[u8]) -> io::Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let mut stdout = io::stdout().lock();

    // Kitty protocol: transmit + display in one go
    // a=T (transmit+display), f=100 (auto-detect format), C=1 (move cursor),
    // c=MAX_COLS (width in columns)
    let chunk_size = 4096;
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(chunk_size)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == chunks.len() - 1;
        let m = i32::from(!is_last);

        if i == 0 {
            write!(
                stdout,
                "\x1b_Ga=T,f=100,C=1,c={MAX_COLS},m={m};{chunk}\x1b\\"
            )?;
        } else {
            write!(stdout, "\x1b_Gm={m};{chunk}\x1b\\")?;
        }
    }

    writeln!(stdout)?;
    stdout.flush()
}

/// Extract Teams image URLs from HTML content.
///
/// Returns URLs for `AMSImage` type images (not emoji).
#[must_use]
pub fn extract_image_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut search_from = 0;

    while let Some(img_start) = html[search_from..].find("<img ") {
        let abs_start = search_from + img_start;
        let Some(img_end) = html[abs_start..].find('>') else {
            break;
        };
        let tag = &html[abs_start..=abs_start + img_end];

        // Only extract AMSImage (actual shared images), not emoji
        if tag.contains("AMSImage")
            && let Some(src) = extract_attr(tag, "src")
            && !src.contains("statics.teams.cdn.office.net")
        {
            urls.push(src);
        }

        search_from = abs_start + img_end + 1;
    }

    urls
}

/// Extract an attribute value from an HTML tag string.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)?;
    let value_start = start + needle.len();
    let end = tag[value_start..].find('"')?;
    Some(tag[value_start..value_start + end].to_string())
}
