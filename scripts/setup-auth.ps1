#!/usr/bin/env pwsh
# Install Playwright and Chromium for tmz browser-based auth (Windows).
#
# This script installs Playwright locally in the same directory as
# teams-auth.mjs. ES modules ignore NODE_PATH, so a global install
# (npm install -g playwright) will NOT work.
#
# Usage: pwsh setup-auth.ps1
#        .\setup-auth.ps1

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $PSCommandPath
$AuthScript = Join-Path $ScriptDir 'teams-auth.mjs'
$PackageJson = Join-Path $ScriptDir 'package.json'

# Verify teams-auth.mjs exists
if (-not (Test-Path $AuthScript)) {
    Write-Error "teams-auth.mjs not found in $ScriptDir. Place this script next to teams-auth.mjs."
    exit 1
}

# Check Node.js is available
$nodePath = Get-Command node -ErrorAction SilentlyContinue
if (-not $nodePath) {
    Write-Host @"
Node.js is not installed or not in PATH.

If winget is available:
  winget install OpenJS.NodeJS.LTS

If winget fails (common on corporate machines), install portable Node.js:
  `$dest = "`$env:USERPROFILE\.local\node"
  Invoke-WebRequest 'https://nodejs.org/dist/v22.14.0/node-v22.14.0-win-x64.zip' -OutFile "`$env:TEMP\node.zip"
  Expand-Archive "`$env:TEMP\node.zip" -DestinationPath "`$env:USERPROFILE\.local\" -Force
  Rename-Item "`$env:USERPROFILE\.local\node-v22.14.0-win-x64" `$dest
  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ";`$dest", 'User')

Then restart your terminal and run this script again.
"@ -ForegroundColor Yellow
    exit 1
}

Write-Host "Node.js found: $($nodePath.Source)" -ForegroundColor Green

# Create package.json if missing
if (-not (Test-Path $PackageJson)) {
    Write-Host "Creating package.json..."
    Set-Content $PackageJson '{"name":"tmz-auth","type":"module","dependencies":{"playwright":"latest"}}' -Encoding UTF8
}

# Install Playwright locally
Write-Host "Installing Playwright (local to $ScriptDir)..."
Push-Location $ScriptDir
try {
    npm install
    if ($LASTEXITCODE -ne 0) {
        Write-Error "npm install failed."
        exit 1
    }

    Write-Host "Installing Chromium browser..."
    npx playwright install chromium
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Playwright Chromium install failed."
        exit 1
    }
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "Setup complete. You can now run: tmz auth login" -ForegroundColor Green
