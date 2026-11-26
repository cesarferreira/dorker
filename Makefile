.PHONY: all build build-release install install-release clean test check fmt lint run demo release publish-aa publish-aa-fresh

LEVEL ?= minor

# Default target
all: check build test

# Build debug version
build:
	cargo build

# Build release version
build-release:
	cargo build --release

# Install debug binary to ~/.cargo/bin
install:
	CARGO_INCREMENTAL=0 cargo install --path . --locked --bins --debug --force

# Install release binary to ~/.cargo/bin
install-release:
	CARGO_INCREMENTAL=0 cargo install --path . --locked --bins --force

# Clean build artifacts
clean:
	cargo clean

# Run tests (needs cargo-nextest: cargo install cargo-nextest --locked)
test:
	cargo nextest run

# End-to-end tests that boot real emulators (about 3 minutes)
test-e2e:
	cargo nextest run --profile e2e --run-ignored only

# Run clippy and check
check:
	cargo check
	cargo clippy -- -D warnings

# Format code
fmt:
	cargo fmt

# Lint (check formatting)
lint:
	cargo fmt -- --check
	cargo clippy -- -D warnings

# Run with arguments (usage: make run ARGS="--hello")
run:
	cargo run -- $(ARGS)

# Quick demo
demo: install
	@echo "=== dorker demo ==="
	dorker --help

# Bump version, regenerate CHANGELOG.md, tag, publish, and push (requires cargo-release + git-cliff)
release:
	cargo release $(LEVEL) --execute --no-confirm

# Publish the Android Auto image (templates/android-auto-with-dev-mode) with the latest
# Android Auto, for this machine's CPU. Run the same command on an Apple Silicon Mac and on
# x86_64 Linux. It installs dorker from this checkout, copies the template into .publish/
# (ignored by git) and runs its publish.sh there, so the checkout stays clean.
#   make publish-aa         update the previous version, like a Play Store update
#   make publish-aa-fresh   build from scratch (a clean install)
# Override the target with AA_REPO=ghcr.io/org/name (or DORKER_PUBLISH_REPO) and AA_API=36.
AA_REPO ?= $(or $(DORKER_PUBLISH_REPO),ghcr.io/cesarferreira/gearhead)
AA_API ?= 37
AA_DIR := .publish/android-auto

publish-aa: install-release
	dorker init --template android-auto-with-dev-mode $(AA_DIR) --force
	$(MAKE) -C $(AA_DIR) publish REPO=$(AA_REPO) API=$(AA_API)

publish-aa-fresh: install-release
	dorker init --template android-auto-with-dev-mode $(AA_DIR) --force
	$(MAKE) -C $(AA_DIR) publish-fresh REPO=$(AA_REPO) API=$(AA_API)
