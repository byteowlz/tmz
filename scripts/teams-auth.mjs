#!/usr/bin/env node
// Teams browser-based token extraction using Playwright.
//
// Launches a Chromium browser, navigates to Teams web client, waits for
// the user to complete authentication, then captures MSAL access tokens
// by intercepting OAuth token responses from login.microsoftonline.com.
//
// Usage:
//   node scripts/teams-auth.mjs [--timeout 300] [--headless]
//
// Output (JSON to stdout):
//   { "skype_token": "...", "chat_token": "...", "graph_token": "...",
//     "presence_token": "...", "expires_in": 3600 }

import { chromium } from "playwright";

const TEAMS_URL = "https://teams.microsoft.com/v2";
const DEFAULT_TIMEOUT_SECS = 300;
const POLL_INTERVAL_MS = 1000;

// Token scopes we need, mapped to our names
const REQUIRED_TOKENS = {
  "api.spaces.skype.com": "skype_token",
  "chatsvcagg.teams.microsoft.com": "chat_token",
  "graph.microsoft.com": "graph_token",
  "presence.teams.microsoft.com": "presence_token",
};

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
  process.stderr.write(`[tmz-auth] ${msg}\n`);
}

async function main() {
  const { timeout, headless } = parseArgs();
  const deadlineMs = Date.now() + timeout * 1000;

  log("Launching browser for Teams authentication...");
  if (!headless) {
    log("Please complete the login in the browser window.");
  }

  const userDataDir =
    process.env.XDG_STATE_HOME
      ? `${process.env.XDG_STATE_HOME}/tmz/browser-profile`
      : process.env.HOME
        ? `${process.env.HOME}/.local/state/tmz/browser-profile`
        : `/tmp/tmz-browser-profile`;

  const context = await chromium.launchPersistentContext(userDataDir, {
    headless,
    channel: "chromium",
    args: ["--disable-blink-features=AutomationControlled"],
    viewport: { width: 1280, height: 900 },
    locale: "en-US",
  });

  const page = context.pages()[0] || (await context.newPage());

  // Capture tokens from OAuth responses and localStorage
  const captured = {};
  let minExpiresIn = Infinity;

  // Strategy 1: Intercept network OAuth token responses
  page.on("response", async (response) => {
    try {
      const url = response.url();
      if (!url.includes("oauth2/v2.0/token") || response.status() !== 200) return;

      const body = await response.json();
      if (!body.access_token) return;

      const scope = (body.scope || "").toLowerCase();
      for (const [resource, name] of Object.entries(REQUIRED_TOKENS)) {
        if (scope.includes(resource) && !captured[name]) {
          captured[name] = body.access_token;
          if (body.expires_in && body.expires_in < minExpiresIn) {
            minExpiresIn = body.expires_in;
          }
          log(`Captured ${name} (via network)`);
        }
      }
    } catch {
      // Ignore response parsing errors
    }
  });

  try {
    await page.goto(TEAMS_URL, { waitUntil: "domcontentloaded" });
    log("Waiting for authentication to complete...");

    while (Date.now() < deadlineMs) {
      await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));

      // Check if we have all tokens from network interception
      if (hasAllTokens(captured)) {
        return finish(captured, minExpiresIn, context);
      }

      // Strategy 2: Check localStorage for Teams v2 tmp.auth format
      const url = page.url();
      const isOnTeams =
        url.includes("teams.microsoft.com") &&
        !url.includes("login.microsoftonline.com") &&
        !url.includes("login.live.com");

      if (!isOnTeams) continue;

      const lsTokens = await extractFromLocalStorage(page);
      for (const [name, token] of Object.entries(lsTokens)) {
        if (!captured[name]) {
          captured[name] = token;
          log(`Captured ${name} (via localStorage)`);
        }
      }

      if (hasAllTokens(captured)) {
        return finish(captured, minExpiresIn, context);
      }

      // Strategy 3: Fallback - old MSAL v1 localStorage format
      const oldTokens = await extractOldMsalFormat(page);
      for (const [name, token] of Object.entries(oldTokens)) {
        if (!captured[name]) {
          captured[name] = token;
          log(`Captured ${name} (via MSAL v1)`);
        }
      }

      if (hasAllTokens(captured)) {
        return finish(captured, minExpiresIn, context);
      }

      const count = Object.keys(captured).length;
      const total = Object.keys(REQUIRED_TOKENS).length;
      if (count > 0) {
        log(`Have ${count}/${total} tokens, waiting for more...`);
      } else {
        log("Waiting for tokens...");
      }
    }

    log("ERROR: Timed out waiting for authentication.");
    await context.close();
    process.exit(1);
  } catch (err) {
    log(`ERROR: ${err.message}`);
    try { await context.close(); } catch {}
    process.exit(1);
  }
}

function hasAllTokens(captured) {
  return Object.values(REQUIRED_TOKENS).every((name) => captured[name]);
}

async function finish(captured, minExpiresIn, context) {
  log("All required tokens captured.");
  const output = { ...captured };
  if (minExpiresIn < Infinity) {
    output.expires_in = minExpiresIn;
  }
  process.stdout.write(JSON.stringify(output));
  await context.close();
  process.exit(0);
}

/** Extract tokens from Teams v2 tmp.auth localStorage format. */
async function extractFromLocalStorage(page) {
  return await page.evaluate((resources) => {
    const tokens = {};
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || !key.includes("Token")) continue;

      for (const [resource, name] of Object.entries(resources)) {
        if (!key.toUpperCase().includes(resource.toUpperCase())) continue;
        try {
          const val = JSON.parse(localStorage.getItem(key));
          const token = val?.item?.token;
          if (token && token !== "dummy-token" && token.length > 50) {
            tokens[name] = token;
          }
        } catch {}
      }
    }
    return tokens;
  }, REQUIRED_TOKENS);
}

/** Extract tokens from old MSAL v1 localStorage format. */
async function extractOldMsalFormat(page) {
  return await page.evaluate((resources) => {
    const tokens = {};
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key || !key.includes("accesstoken") || !key.includes("login.windows.net")) continue;

      for (const [resource, name] of Object.entries(resources)) {
        if (!key.toLowerCase().includes(resource.toLowerCase())) continue;
        try {
          const val = JSON.parse(localStorage.getItem(key));
          if (val?.secret && val.secret.length > 50) {
            tokens[name] = val.secret;
          }
        } catch {}
      }
    }
    return tokens;
  }, REQUIRED_TOKENS);
}

main();
