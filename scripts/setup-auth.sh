#!/usr/bin/env bash
set -euo pipefail

# Install Playwright and Chromium for tmz browser-based auth.
# Run this once before using `tmz-cli auth login`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Installing Node.js dependencies..."
cd "$SCRIPT_DIR"
npm install

echo "Installing Chromium browser for Playwright..."
npx playwright install chromium

echo ""
echo "Setup complete. You can now run: tmz-cli auth login"
