#!/usr/bin/env node
// Teams browser-based token extraction using Playwright.
//
// Launches Chromium with a persistent profile, navigates to Teams,
// and extracts MSAL access tokens from localStorage.
//
// If the cached browser profile has stale tokens, the MSAL cache is
// cleared and the page reloaded to force a fresh SSO authentication.
//
// Usage:
//   node teams-auth.mjs [--timeout 300] [--headless]
//
// Output (JSON to stdout):
//   { "<localStorage-key>": "<localStorage-value>", ... }

import { chromium } from "playwright";

const TEAMS_URL = "https://teams.microsoft.com/v2";
const DEFAULT_TIMEOUT_SECS = 300;
const POLL_INTERVAL_MS = 2000;

const REQUIRED_RESOURCES = [
  "api.spaces.skype.com",
  "chatsvcagg.teams.microsoft.com",
  "graph.microsoft.com",
  "presence.teams.microsoft.com",
];

const REQUIRED_CAPTURED_FIELDS = [
  "skype_token",
  "chat_token",
  "graph_token",
  "presence_token",
];

const MIN_CAPTURED_FIELDS = ["skype_token"];

function parseArgs() {
  const args = process.argv.slice(2);
  let timeout = DEFAULT_TIMEOUT_SECS;
  let headless = false;
  let fresh = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--timeout" && args[i + 1]) {
      timeout = parseInt(args[i + 1], 10);
      i++;
    } else if (args[i] === "--headless") {
      headless = true;
    } else if (args[i] === "--fresh") {
      fresh = true;
    }
  }
  return { timeout, headless, fresh };
}

function log(msg) {
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

/** Clear all MSAL and Teams auth caches from localStorage. */
async function clearAuthCaches(page) {
  return await page.evaluate(() => {
    const keysToRemove = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (!key) continue;
      // Remove old MSAL format tokens
      if (key.includes("accesstoken") || key.includes("idtoken") || key.includes("refreshtoken")) {
        keysToRemove.push(key);
      }
      // Remove new Teams v2 tmp.auth tokens
      if (key.startsWith("tmp.auth.") && key.includes(".Token.")) {
        keysToRemove.push(key);
      }
      // Remove MSAL token key indices
      if (key.includes("msal.token.keys")) {
        keysToRemove.push(key);
      }
    }
    keysToRemove.forEach((k) => localStorage.removeItem(k));
    return keysToRemove.length;
  });
}

function normalizeBearerToken(headerValue) {
  if (!headerValue || typeof headerValue !== "string") return null;
  const trimmed = headerValue.trim();
  if (!trimmed) return null;
  return trimmed.replace(/^bearer\s+/i, "").trim() || null;
}

function hasAllCapturedTokens(tokens) {
  return REQUIRED_CAPTURED_FIELDS.every((field) => {
    const val = tokens[field];
    return typeof val === "string" && val.length > 16;
  });
}

function hasMinimumCapturedTokens(tokens) {
  return MIN_CAPTURED_FIELDS.every((field) => {
    const val = tokens[field];
    return typeof val === "string" && val.length > 16;
  });
}

function buildScriptTokenOutput(tokens) {
  return {
    skype_token: tokens.skype_token || "",
    chat_token: tokens.chat_token || "",
    graph_token: tokens.graph_token || "",
    presence_token: tokens.presence_token || "",
  };
}

async function main() {
  const { timeout, headless, fresh } = parseArgs();
  const deadlineMs = Date.now() + timeout * 1000;

  log(headless ? "Headless token refresh..." : "Launching browser...");

  const userDataDir =
    process.env.XDG_STATE_HOME
      ? `${process.env.XDG_STATE_HOME}/tmz/browser-profile`
      : process.env.HOME
        ? `${process.env.HOME}/.local/state/tmz/browser-profile`
        : `/tmp/tmz-browser-profile`;

  // --fresh: nuke the browser profile to force clean login
  if (fresh) {
    const fs = await import("fs");
    try {
      fs.rmSync(userDataDir, { recursive: true, force: true });
      log("Cleared browser profile for fresh login.");
    } catch {}
  }

  const context = await chromium.launchPersistentContext(userDataDir, {
    headless,
    channel: "chromium",
    args: ["--disable-blink-features=AutomationControlled"],
    viewport: { width: 1280, height: 900 },
    locale: "en-US",
  });

  const page = context.pages()[0] || (await context.newPage());

  const capturedTokens = {
    skype_token: null,
    chat_token: null,
    graph_token: null,
    presence_token: null,
  };

  context.on("request", (request) => {
    try {
      const authHeader = request.headers()["authorization"];
      const token = normalizeBearerToken(authHeader);
      if (!token) return;

      const url = request.url().toLowerCase();

      // Teams authz exchange uses the Skype access token as bearer.
      if (url.includes("teams.microsoft.com/api/authsvc/v1.0/authz") && !capturedTokens.skype_token) {
        capturedTokens.skype_token = token;
        return;
      }

      if (url.includes("chatsvcagg.teams.microsoft.com") && !capturedTokens.chat_token) {
        capturedTokens.chat_token = token;
        return;
      }

      if (url.includes("graph.microsoft.com") && !capturedTokens.graph_token) {
        capturedTokens.graph_token = token;
        return;
      }

      if (url.includes("presence.teams.microsoft.com") && !capturedTokens.presence_token) {
        capturedTokens.presence_token = token;
      }
    } catch {
      // ignore request inspection errors
    }
  });

  try {
    await page.goto(TEAMS_URL, { waitUntil: "domcontentloaded" });
    log("Waiting for authentication...");

    let clearedCache = false;
    // On Teams but no tokens after this many polls -> clear cache
    let onTeamsPolls = 0;
    const STALE_THRESHOLD = 5; // 10 seconds
    let localStorageReadySince = null;

    while (Date.now() < deadlineMs) {
      await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));

      let url;
      try {
        url = page.url();
      } catch {
        continue; // page navigating
      }

      const isOnLogin =
        url.includes("login.microsoftonline.com") ||
        url.includes("login.live.com");

      if (isOnLogin) {
        onTeamsPolls = 0;
        // In headless mode, if we're on a login page after clearing cache,
        // SSO cookies have expired. Fail fast.
        if (headless && clearedCache) {
          log("ERROR: SSO session expired. Run 'tmz auth login' interactively.");
          await context.close();
          process.exit(1);
        }
        log("On login page, waiting for SSO...");
        continue;
      }

      let tokens;
      try {
        tokens = await extractAccessTokens(page);
      } catch {
        continue; // context destroyed during navigation
      }

      if (hasAllCapturedTokens(capturedTokens)) {
        log("All tokens extracted (network capture).");
        process.stdout.write(JSON.stringify(buildScriptTokenOutput(capturedTokens)));
        await context.close();
        process.exit(0);
      }

      if (hasAllRequiredTokens(tokens)) {
        if (!localStorageReadySince) {
          localStorageReadySince = Date.now();
        }

        // Give request-capture path a short grace period before minimal-output fallback.
        if (Date.now() - localStorageReadySince >= 6000 && hasMinimumCapturedTokens(capturedTokens)) {
          log("Using minimum captured tokens (skype_token) with optional token gaps.");
          process.stdout.write(JSON.stringify(buildScriptTokenOutput(capturedTokens)));
          await context.close();
          process.exit(0);
        }

        log("Required localStorage entries found, waiting for network token capture...");
      } else {
        localStorageReadySince = null;
      }

      onTeamsPolls++;

      // If we're on Teams but have no tokens after threshold, the
      // cached profile has stale MSAL state. Clear it and reload
      // to force fresh SSO auth.
      if (!clearedCache && onTeamsPolls >= STALE_THRESHOLD) {
        try {
          const removed = await clearAuthCaches(page);
          log(`Cleared ${removed} stale auth cache entries, reloading...`);
          await page.reload({ waitUntil: "domcontentloaded" });
          clearedCache = true;
          onTeamsPolls = 0;
        } catch {
          // ignore errors during reload
        }
        continue;
      }

      const count = Object.keys(tokens).length;
      if (count > 0) {
        log(`${count} tokens found, waiting for all ${REQUIRED_RESOURCES.length}...`);
      }
    }

    if (hasMinimumCapturedTokens(capturedTokens)) {
      log("Timed out before full token set; using captured minimum token set.");
      process.stdout.write(JSON.stringify(buildScriptTokenOutput(capturedTokens)));
      await context.close();
      process.exit(0);
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

main();
