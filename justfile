# Use PowerShell on Windows, default sh on Linux/macOS
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# List available recipes by default
[private]
default:
  @just --list --unsorted


# --- Development & Run ---

# Build and run the application in development mode
[group: 'Development & Run']
dev:
  cargo run

# Build and run the application in release mode
[group: 'Development & Run']
prod:
  cargo run --release


# --- Build & Distribution ---

# Build release binary (optionally baking in WEPICS_DATA_DIR)
[group: 'Build & Distribution']
[arg('data_dir', long='data-dir')]
build data_dir="":
  {{ if data_dir != "" { 
  "cargo build --release --config 'env.WEPICS_DATA_DIR=\"" + data_dir + "\"'" 
  } else { 
  "cargo build --release" 
  } }}

# Package the installer
[group: 'Build & Distribution']
package:
  cargo packager --release

# Build release binary and package the installer
[group: 'Build & Distribution']
[arg('data_dir', long='data-dir')]
dist data_dir="wepics-data": (build data_dir) package


# --- Code Quality & Testing ---

# Check main code for compilation errors
[group: 'Code Quality & Testing']
check:
  cargo check

# Check all targets (main code, tests, examples...) for compilation errors
[group: 'Code Quality & Testing']
check-all:
  cargo check --all-targets

# Run all tests across the workspace
[group: 'Code Quality & Testing']
test:
  cargo test

# Run Clippy linter with strict warnings
[group: 'Code Quality & Testing']
lint:
  cargo clippy --all-targets --all-features -- -D warnings


# --- Dependencies ---

# Download project dependencies to local cache without compiling
[group: 'Dependencies']
fetch:
  cargo fetch

# Pre-compile all project dependencies (warms up target/ cache)
[group: 'Dependencies']
deps-build:
  cargo test --no-run

# Check for newer compatible versions in Cargo.lock
[group: 'Dependencies']
deps-check:
  cargo update --dry-run

# Check for major version updates in Cargo.toml
[group: 'Dependencies']
outdated:
  cargo outdated --root-deps-only

# Check Cargo.lock for known security vulnerabilities (CVE / RustSec)
[group: 'Dependencies']
audit:
  cargo audit

# Update dependencies to latest compatible versions in Cargo.lock
[group: 'Dependencies']
update:
  cargo update

# Display the dependency graph
[group: 'Dependencies']
tree:
  cargo tree


# --- Environment diagnostics ---

# Check production environment (media handlers: vips, ffmpeg, ffprobe)
[group: 'Environment']
doctor: _doctor-media

# Check development environment (Rust toolchain, packager, media handlers)
[group: 'Environment']
doctor-dev: _doctor-media _doctor-dev


# --- Setup environment ---

# Install production environment: media handlers for image and video
[group: 'Environment']
setup: setup-media-handlers

# Install development environment: Rust toolchain, media handlers and packaging tools
[group: 'Environment']
setup-dev: setup-rust setup-media-handlers setup-audit-tools setup-packager


# --- Setup environment: production parts ---

# Install media handlers for image and video
[group: 'Environment']
setup-media-handlers: setup-image-handler setup-video-handler

# Install image handler (libVIPS) for thumbnailing  static pictures
[group: 'Environment']
setup-image-handler: _setup-image-handler

# Install video handler (FFMpeg) for extract metainfo, extract preview frames and preview transcoding
[group: 'Environment']
setup-video-handler: _setup-video-handler


# --- Setup environment: development parts ---

# Install Rust toolchain (cargo, rustc, rustup)
[group: 'Environment']
setup-rust: _setup-rust

# Install dependency inspection tools (cargo-audit, cargo-outdated)
[group: 'Environment']
setup-audit-tools:
  cargo install cargo-audit cargo-outdated --locked

# Install packaging tools
[group: 'Environment']
setup-packager:
  cargo install cargo-packager --locked


# ========= OS-SPECIFIC =========

# --- Rust installation (OS-specific) ---

[unix]
[private]
_setup-rust:
  @command -v cargo >/dev/null 2>&1 || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

[windows]
[private]
_setup-rust:
  @if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { winget install --id Rustlang.Rustup -e --source winget --accept-package-agreements --accept-source-agreements }


# --- Image handler: libvips (OS-specific) ---

[macos]
[private]
_setup-image-handler:
  @command -v vips >/dev/null 2>&1 || brew install vips

[linux]
[private]
_setup-image-handler:
  @command -v vips >/dev/null 2>&1 || (sudo apt-get update && sudo apt-get install -y libvips-tools)

[windows]
[private]
_setup-image-handler:
  @if (-not (Get-Command scoop -ErrorAction SilentlyContinue)) { irm get.scoop.sh | iex }
  @if (-not (Get-Command vips -ErrorAction SilentlyContinue)) { scoop install libvips }


# --- Video handler: ffmpeg + ffprobe (OS-specific) ---

[macos]
[private]
_setup-video-handler:
  @command -v ffmpeg >/dev/null 2>&1 || brew install ffmpeg

[linux]
[private]
_setup-video-handler:
  @command -v ffmpeg >/dev/null 2>&1 || (sudo apt-get update && sudo apt-get install -y ffmpeg)

[windows]
[private]
_setup-video-handler:
  @if (-not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) { winget install --id Gyan.FFmpeg -e --source winget --accept-package-agreements --accept-source-agreements }


# --- Media tools check (OS-specific) ---

[unix]
[private]
_doctor-media:
  #!/usr/bin/env bash
  set -euo pipefail
  ok=true
  for cmd in vips ffmpeg ffprobe; do
  if command -v "$cmd" >/dev/null 2>&1; then
  ver=$("$cmd" -version 2>&1 | head -n 1)
  printf "  \033[32m✓\033[0m %-10s %s\n" "$cmd" "$ver"
  else
  printf "  \033[31m✗\033[0m %-10s NOT FOUND\n" "$cmd"
  ok=false
  fi
  done
  $ok

[windows]
[private]
_doctor-media:
  @foreach ($cmd in @('vips', 'ffmpeg', 'ffprobe')) { \
  if (Get-Command $cmd -ErrorAction SilentlyContinue) { \
  $ver = (& $cmd -version 2>&1 | Select-Object -First 1); \
  Write-Host "  [OK] $($cmd.PadRight(10)) $ver" -ForegroundColor Green \
  } else { \
  Write-Host "  [MISSING] $($cmd.PadRight(10)) NOT FOUND" -ForegroundColor Red; \
  $script:failed = $true \
  } \
  }; if ($script:failed) { exit 1 }


# --- Dev tools check (OS-specific) ---

[unix]
[private]
_doctor-dev:
  #!/usr/bin/env bash
  set -euo pipefail
  ok=true
  for cmd in rustc cargo cargo-packager; do
  if command -v "$cmd" >/dev/null 2>&1; then
  ver=$("$cmd" --version 2>&1 | head -n 1)
  printf "  \033[32m✓\033[0m %-15s %s\n" "$cmd" "$ver"
  else
  printf "  \033[31m✗\033[0m %-15s NOT FOUND\n" "$cmd"
  ok=false
  fi
  done
  $ok

[windows]
[private]
_doctor-dev:
  @foreach ($cmd in @('rustc', 'cargo', 'cargo-packager')) { \
  if (Get-Command $cmd -ErrorAction SilentlyContinue) { \
  $ver = (& $cmd --version 2>&1 | Select-Object -First 1); \
  Write-Host "  [OK] $($cmd.PadRight(15)) $ver" -ForegroundColor Green \
  } else { \
  Write-Host "  [MISSING] $($cmd.PadRight(15)) NOT FOUND" -ForegroundColor Red; \
  $script:failed = $true \
  } \
  }; if ($script:failed) { exit 1 }