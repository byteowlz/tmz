# tmz justfile (Rust workspace)

# Default recipe - show help
default:
    @just --list

# === Development ===

# Data directory for installed resources
data_dir := env("XDG_DATA_HOME", env("HOME", "~") / ".local/share") / "tmz"

# Install all binaries + auth script
install: install-all

# Install all binaries from workspace and supporting files
install-all:
    @for crate in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.targets[] | .kind[] == "bin") | .manifest_path | split("/") | .[-2]'); do \
        echo "Installing $crate..."; \
        cargo install --path crates/$crate; \
    done
    @just install-scripts

# Install a specific crate
install-crate CRATE:
    cargo install --path crates/{{CRATE}}

# Install auth script and Node dependencies to data dir
install-scripts:
    @echo "Installing auth scripts to {{data_dir}}..."
    @mkdir -p "{{data_dir}}"
    @cp scripts/teams-auth.mjs "{{data_dir}}/teams-auth.mjs"
    @cp scripts/package.json "{{data_dir}}/package.json"
    @cd "{{data_dir}}" && npm install --silent 2>/dev/null && npx playwright install chromium 2>/dev/null
    @echo "Auth script installed to {{data_dir}}/teams-auth.mjs"

# Uninstall all binaries
uninstall:
    @for crate in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.targets[] | .kind[] == "bin") | .name'); do \
        echo "Uninstalling $crate..."; \
        cargo uninstall $crate 2>/dev/null || true; \
    done

# === Building ===

# Debug build (all crates)
build:
    cargo build --workspace

# Release build (all crates)
build-release:
    cargo build --workspace --release

# Build a specific crate
build-crate CRATE:
    cargo build -p {{CRATE}}

# Build with all features
build-all:
    cargo build --workspace --all-features

# Fast compile check
check:
    cargo check --workspace

# Check a specific crate
check-crate CRATE:
    cargo check -p {{CRATE}}

# Clean build artifacts
clean:
    cargo clean

# === Testing ===

# Run all tests
test:
    cargo test --workspace

# Run tests for a specific crate
test-crate CRATE:
    cargo test -p {{CRATE}}

# Run tests with all features
test-all:
    cargo test --workspace --all-features

# Run tests verbosely
test-v:
    cargo test --workspace -- --nocapture

# Run a specific test
test-one TEST:
    cargo test --workspace {{TEST}}

# === Code Quality ===

# Format all code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Run clippy on all crates
clippy:
    cargo clippy --workspace -- -D warnings

# Alias for clippy
lint: clippy

# Clippy on a specific crate
clippy-crate CRATE:
    cargo clippy -p {{CRATE}} -- -D warnings

# Auto-fix clippy warnings
fix:
    cargo clippy --workspace --fix --allow-dirty

# Run all checks
check-all: fmt-check clippy test

# === Auth Setup ===

# Install Playwright and Chromium for browser-based auth
setup-auth:
    ./scripts/setup-auth.sh

# === Config Generation ===

# Generate config.toml and schema from Rust structs
generate-config:
    cargo run -p tmz-core --example generate_config

# Validate that examples/ config files are up to date
validate-config:
    cargo test -p tmz-core validate_examples_are_up_to_date

# === Documentation ===

# Generate docs for all crates
docs:
    cargo doc --workspace --no-deps

# Generate and open docs
docs-open:
    cargo doc --workspace --no-deps --open

# Docs for a specific crate
docs-crate CRATE:
    cargo doc -p {{CRATE}} --no-deps --open

# === Dependencies ===

# Update all dependencies
update:
    cargo update

# Check for outdated dependencies
outdated:
    cargo outdated --workspace

# === Workspace Info ===

# List all crates in workspace
list:
    @cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | "\(.name) (\(.version))"'

# List binary crates
list-bins:
    @cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.targets[] | .kind[] == "bin") | .name'

# List library crates
list-libs:
    @cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.targets[] | .kind[] == "lib") | .name'

# === Release ===

# Release build and show binary sizes
release: build-release
    @echo "Binary sizes:"
    @find target/release -maxdepth 1 -type f -perm +111 ! -name "*.d" -exec ls -lh {} \; 2>/dev/null || true

# Bump version: just bump patch|minor|major
bump LEVEL:
    #!/usr/bin/env bash
    set -euo pipefail
    current=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    IFS='.' read -r major minor patch <<< "$current"
    case "{{LEVEL}}" in
        patch) patch=$((patch + 1)) ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        major) major=$((major + 1)); minor=0; patch=0 ;;
        *) echo "Usage: just bump patch|minor|major"; exit 1 ;;
    esac
    new="${major}.${minor}.${patch}"
    sed -i '' "s/^version = \"${current}\"/version = \"${new}\"/" Cargo.toml
    cargo check --workspace 2>/dev/null
    git add -A
    git commit -m "chore: bump to ${new}"
    echo "Bumped ${current} -> ${new}"

# Tag and push a release
release-tag VERSION:
    git tag v{{VERSION}}
    git push --tags

# Bump, tag, and push in one step: just release-bump patch|minor|major
release-bump LEVEL:
    #!/usr/bin/env bash
    set -euo pipefail
    just bump {{LEVEL}}
    version=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    git tag "v${version}"
    git push origin main
    git push origin "v${version}"
    echo "Released v${version}"

# Set up GitHub secrets for automated releases (requires byt)
setup-secrets:
    byt secrets setup tmz
