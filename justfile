# Use PowerShell on Windows, default sh on Linux/macOS
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# List available recipes by default
default:
  @just --list

# Update minor dependencies version
update:
  cargo update

# Check new version of dependencies
deps:
  cargo update --dry-run

# Run the application in development mode
dev:
  cargo run

# Build and run the application in release mode
prod:
  cargo run --release

# Build release binary
build:
  cargo build --release

# Package the installer
package:
  cargo packager --release

# Build release binary and package the installer
dist:
  @just build
  @just package

# Check main code for compilation errors
check:
  cargo check

# Check all targets (main code, tests, examples...) for compilation errors
check-all:
  cargo check --all-targets

# Run all tests across the workspace
test:
  cargo test

# Run Clippy linter with strict warnings
lint:
  cargo clippy --all-targets --all-features -- -D warnings

# Install required packaging CLI tool
setup:
  cargo install cargo-packager --locked
