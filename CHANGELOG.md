# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Changed
- Added ast-grep Rust AI guardrails (`.ast-grep/` rules + `scripts/lint/rust-ai-guardrails.sh`) aligned with the hstry linting setup.
- Updated `just lint` and `just check-all` to include ast-grep guardrail scanning alongside Clippy.
- Added `just install-ast-grep` for one-command ast-grep installation.

### Fixed
- Made Teams auth token extraction resilient to updated MSAL localStorage payload formats (not only `secret`).
- Fixed unstable token selection when multiple access-token entries exist by preferring Teams client-id scoped entries.
- Improved fallback parsing for browser-extracted tokens to reduce false `session expired` errors after successful login.
- Added automatic `tmz auth login` recovery: if normal login fails, tmz now retries once with a fresh browser profile (equivalent to `--fresh`).
- Hardened browser token extraction to handle nested, double-encoded, URL-encoded, and `Bearer`-prefixed token payload variants.
- Added fallback handling for opaque/non-JWT access-token payloads so login can still persist tokens when claims cannot be decoded from the Skype token.
- Updated `just install-all` / `just install-crate` to use `cargo install --force` so local binaries are always refreshed during reinstall.
- Improved Playwright auth script to capture bearer tokens from live Teams network requests and prefer these explicit tokens over brittle localStorage-only extraction.
- Updated token extraction to accept encrypted JWE-style access tokens (5-segment format) in addition to JWTs, preventing false fallback to invalid metadata payloads.
- Switched Playwright fallback behavior to output captured network tokens (minimum `skype_token`) instead of raw localStorage blobs when MSAL cache entries are encrypted (`{id, nonce, data}`).
- Made script-output token ingestion accept optional chat/graph/presence tokens so chat auth can persist even when only Skype token capture is available.

