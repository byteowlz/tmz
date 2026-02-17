#!/usr/bin/env node
// Teams browser-based token extraction using Playwright.
//
// Launches a Chromium browser, navigates to Teams web client, waits for
// the user to complete authentication (SSO, MFA, etc.), then extracts
// MSAL access tokens from localStorage and outputs them as JSON to stdout.
//
// Usage:
//   node scripts/teams-auth.mjs [--timeout 300] [--headless]
//
// Output (JSON to stdout):
//   { "<localStorage-key>": "<localStorage-value>", ... }
//
// Only keys containing "accesstoken" and "login.windows.net" are included.

import { chromium } from "playwright";

const TEAMS_URL = "https://teams.microsoft.com/v2";

// Timeout in seconds (default: 5 minutes for slow SSO/MFA flows)
const DEFAULT_TIMEOUT_SECS = 300;

// Poll interval in ms
const POLL_INTERVAL_MS = 2000;

// Resources we need tokens for
const REQUIRED_RESOURCES = [
  "api.spaces.skype.com",
  "chatsvcagg.teams.microsoft.com",
  "graph.microsoft.com",
  "presence.teams.microsoft.com",
];

function parseArgs() {
  const args = process.argv.slice(2);
  let timeout = DEFAULT_TIMEOUT_SECS;
  let headless = false;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--timeout" && args[i + 1]) {
      timeout = parseInt(args[i + 1], 10);
      i++;
    } else if (args[i] === "--headless") {
      headless = true;
    }
  }

  return { timeout, headless };
}

function log(msg) {
  // Log to stderr so stdout stays clean for JSON output
  process.stderr.write(`[tmz-auth] ${msg}\n`);
}

async function extractAccessTokens(page) {
  return await page.evaluate(() => {
    const tokens = {};
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (
        key &&
        key.includes("accesstoken") &&
        key.includes("login.windows.net")
      ) {
        tokens[key] = localStorage.getItem(key);
      }
    }
    return tokens;
  });
}

function hasAllRequiredTokens(tokens) {
  const keys = Object.keys(tokens);
  return REQUIRED_RESOURCES.every((resource) =>
    keys.some((key) => key.toLowerCase().includes(resource.toLowerCase()))
  );
}

async function main() {
  const { timeout, headless } = parseArgs();
  const deadlineMs = Date.now() + timeout * 1000;

  log("Launching browser for Teams authentication...");
  log("Please complete the login in the browser window.");

  // Use a persistent context so cookies/sessions survive between runs.
  // This also means subsequent logins may be faster (SSO cookie cached).
  const userDataDir =
    process.env.XDG_STATE_HOME
      ? `${process.env.XDG_STATE_HOME}/tmz/browser-profile`
      : process.env.HOME
        ? `${process.env.HOME}/.local/state/tmz/browser-profile`
        : `/tmp/tmz-browser-profile`;

  const context = await chromium.launchPersistentContext(userDataDir, {
    headless,
    channel: "chromium",
    args: [
      "--disable-blink-features=AutomationControlled",
    ],
    viewport: { width: 1280, height: 900 },
    locale: "en-US",
  });

  const page = context.pages()[0] || (await context.newPage());

  try {
    await page.goto(TEAMS_URL, { waitUntil: "domcontentloaded" });

    log("Waiting for authentication to complete...");

    // Poll localStorage for access tokens
    while (Date.now() < deadlineMs) {
      await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));

      // Check if we're on the Teams app (not a login page)
      const url = page.url();
      const isOnTeams =
        url.includes("teams.microsoft.com") &&
        !url.includes("login.microsoftonline.com") &&
        !url.includes("login.live.com");

      if (!isOnTeams) {
        continue;
      }

      const tokens = await extractAccessTokens(page);

      if (hasAllRequiredTokens(tokens)) {
        log("All required tokens extracted successfully.");
        // Output tokens as JSON to stdout
        process.stdout.write(JSON.stringify(tokens));
        await context.close();
        process.exit(0);
      }

      // We're on Teams but tokens not ready yet - MSAL may still be fetching
      log("On Teams page, waiting for tokens to populate...");
    }

    log("ERROR: Timed out waiting for authentication.");
    await context.close();
    process.exit(1);
  } catch (err) {
    log(`ERROR: ${err.message}`);
    try {
      await context.close();
    } catch {
      // ignore cleanup errors
    }
    process.exit(1);
  }
}

main();
