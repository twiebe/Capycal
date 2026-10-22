.PHONY: help build build-ui build-gtk run run-ui run-app run-gtk test test-ui test-gtk test-caldav test-caldav-docker test-eventkit test-keychain \
       fmt lint clippy clippy-ui clippy-gtk notices check-notices clean \
       release release-ui release-tag dist dist-macos app dmg dist-linux-x86_64 dist-linux-arm64 dist-linux \
       docker-build up down logs reload docker-build-dev up-dev down-dev logs-dev validate once \
       docs docs-serve docs-docker docs-serve-docker

UNAME_S := $(shell uname -s)
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
DIST    := dist
MACOS_TARGET := aarch64-apple-darwin
CARGO_TARGET_DIR ?= target

# Lists every target with a "## " comment, grouped by the "##@" headings
help: ## Show this help
	@awk 'BEGIN { FS = ":.*## " } /^##@/ { printf "\n%s\n", substr($$0, 5) } /^[a-z0-9_-]+:.*## / { printf "  %-20s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

##@ Development

build: ## Build the CLI
	cargo build

run: ## Run the CLI daemon with capycal.example.toml
	cargo run -- --config capycal.example.toml

validate: ## Validate capycal.example.toml
	cargo run -- --config capycal.example.toml --validate

once: ## Sync capycal.example.toml once
	cargo run -- --config capycal.example.toml --once

build-ui: ## Build with the macOS menu bar app (feature ui)
	cargo build --features ui

# No bundle: no app icon, Start at Login falls back to a LaunchAgent
run-ui: ## Run the macOS menu bar app as a plain binary
	cargo build --features ui --bin capycal-ui
	$(CARGO_TARGET_DIR)/debug/capycal-ui

run-app: app ## Build Capycal.app and open it (macOS)
	open $(DIST)/Capycal.app

# Needs the GTK 4 and libadwaita development files. On macOS (Homebrew gtk4
# and libadwaita) it builds without the tray
build-gtk: ## Build the Linux desktop app (feature gtk)
	cargo build --features gtk --bin capycal-app

# Homebrew's GTK needs its data dirs for GSettings schemas (file dialogs)
run-gtk: ## Run the Linux desktop app
	cargo build --features gtk --bin capycal-app
ifeq ($(UNAME_S),Darwin)
	XDG_DATA_DIRS="$$(brew --prefix)/share" $(CARGO_TARGET_DIR)/debug/capycal-app
else
	$(CARGO_TARGET_DIR)/debug/capycal-app
endif

##@ Tests and checks (lint, test and test-ui check what CI checks without GTK)

test: ## Unit tests
	cargo test --locked

test-ui: ## Tests of the menu bar app
	cargo test --locked --features ui --bin capycal-ui

test-gtk: ## Tests of the Linux desktop app
	cargo test --locked --features gtk --bin capycal-app

test-caldav: ## End-to-end tests against a throwaway Radicale server (needs python3, curl)
	scripts/test-caldav.sh

# The source is mounted read-only; build output and downloads live in Docker
# volumes
test-caldav-docker: ## test-caldav in a Linux container, needs only Docker
	docker run --rm \
		-v "$(CURDIR)":/src:ro -w /src \
		-v capycal-test-cargo:/usr/local/cargo/registry \
		-v capycal-test-target:/target -e CARGO_TARGET_DIR=/target \
		-e DEBIAN_FRONTEND=noninteractive \
		rust:1.99-slim-trixie \
		sh -c 'apt-get update -qq && apt-get install -y -qq --no-install-recommends python3 python3-venv curl >/dev/null && scripts/test-caldav.sh'

# Run from Terminal. They only use new calendars named capycal-test-... and
# remove them afterwards
test-eventkit: ## Tests against Calendar.app (macOS)
	cargo test --lib eventkit::client::live_tests -- --ignored --test-threads=1

test-keychain: ## Read a password from the macOS Keychain or the Secret Service (needs a desktop session)
ifeq ($(shell uname),Darwin)
	security add-generic-password -U -s app.capycal -a capycal-test-live -w capycal-test
else
	printf capycal-test | secret-tool store --label capycal-test service app.capycal username capycal-test-live
endif
	cargo test --lib keychain::tests::live -- --ignored

fmt: ## Format the code
	cargo fmt

lint: ## Check formatting and run clippy, without and with the app
	cargo fmt --check
	$(MAKE) clippy clippy-ui

clippy: ## Clippy without warnings
	cargo clippy --locked --all-targets -- -D warnings

clippy-ui: ## Clippy with the menu bar app
	cargo clippy --locked --all-targets --features ui -- -D warnings

clippy-gtk: ## Clippy with the Linux desktop app
	cargo clippy --locked --features gtk --bin capycal-app --tests -- -D warnings

notices: ## Regenerate THIRD-PARTY-NOTICES.txt after a dependency change (needs cargo-about)
	cargo fetch --locked
	cargo about generate --all-features --frozen about.hbs -o THIRD-PARTY-NOTICES.txt

check-notices: ## Fail if THIRD-PARTY-NOTICES.txt is out of date or a license is not accepted
	cargo fetch --locked
	mkdir -p $(CARGO_TARGET_DIR)
	cargo about generate --all-features --frozen about.hbs -o $(CARGO_TARGET_DIR)/notices.txt
	cmp -s THIRD-PARTY-NOTICES.txt $(CARGO_TARGET_DIR)/notices.txt \
		|| { echo "THIRD-PARTY-NOTICES.txt is out of date: run make notices"; exit 1; }

clean: ## Remove build output and dist/
	cargo clean
	rm -rf $(DIST)

##@ Release and distribution (results land in dist/)

release: ## Release build of the CLI
	cargo build --release --locked

release-ui: ## Release build with the menu bar app
	cargo build --release --locked --features ui

# Pushing the tag starts the release workflow (docs/src/development/releasing.md)
release-tag: ## Set the version in Cargo.toml, commit and tag it: make release-tag VERSION=0.2.0
	@test "$(origin VERSION)" = "command line" || { echo "usage: make release-tag VERSION=x.y.z"; exit 1; }
	@git diff --quiet HEAD || { echo "commit or stash your changes first"; exit 1; }
	VERSION=$(VERSION) perl -0pi -e 's/^version = "[^"]*"/version = "$$ENV{VERSION}"/m' Cargo.toml
	cargo update --workspace --offline --quiet
	git commit --quiet -m "Release $(VERSION)" Cargo.toml Cargo.lock
	git tag -a v$(VERSION) -m "capycal $(VERSION)"
	@echo "Tagged v$(VERSION). Start the release with: git push origin main v$(VERSION)"

ifeq ($(UNAME_S),Darwin)
dist: dist-macos dmg dist-linux ## Everything this host can build
else
dist: dist-linux
endif

dist-macos: ## CLI for Apple silicon (macOS)
	cargo build --release --locked --target $(MACOS_TARGET)
	mkdir -p $(DIST)/$(MACOS_TARGET)
	cp $(CARGO_TARGET_DIR)/$(MACOS_TARGET)/release/capycal $(DIST)/$(MACOS_TARGET)/

app: ## Capycal.app (macOS)
	packaging/macos/build-app.sh $(DIST)

dmg: app ## The dmg with Capycal.app (macOS)
	packaging/macos/build-dmg.sh $(DIST)/Capycal.app $(DIST)/capycal-v$(VERSION)-macos.dmg

# Static musl builds in Docker. A foreign architecture runs emulated, which is
# slow but needs no cross toolchain
dist-linux-x86_64: ## Static Linux CLI for x86_64 (Docker buildx)
	docker buildx build --platform linux/amd64 --build-arg TARGET=x86_64-unknown-linux-musl \
		-f packaging/linux/Dockerfile.musl --output $(DIST)/x86_64-unknown-linux-musl .

dist-linux-arm64: ## Static Linux CLI for arm64 (Docker buildx)
	docker buildx build --platform linux/arm64 --build-arg TARGET=aarch64-unknown-linux-musl \
		-f packaging/linux/Dockerfile.musl --output $(DIST)/aarch64-unknown-linux-musl .

dist-linux: dist-linux-x86_64 dist-linux-arm64 ## Both static Linux CLIs

##@ Docker (docker-compose.yml)

docker-build: ## Build the image
	docker compose build

up: ## Start capycal
	docker compose up -d

down: ## Stop capycal
	docker compose down

logs: ## Follow the logs
	docker compose logs -f capycal

reload: ## Reload the config (SIGHUP)
	docker compose kill -s HUP capycal

##@ Docker for development (docker-compose.dev.yml, Radicale on port 5232)

docker-build-dev: ## Build the dev images
	docker compose -f docker-compose.dev.yml build

up-dev: ## Start the dev setup
	docker compose -f docker-compose.dev.yml up -d

down-dev: ## Stop the dev setup
	docker compose -f docker-compose.dev.yml down

logs-dev: ## Follow the dev logs
	docker compose -f docker-compose.dev.yml logs -f

##@ Documentation

docs: ## Build the book into docs/book/ (needs mdbook)
	mdbook build docs

docs-serve: ## Serve the book on http://localhost:3000 with live reload (needs mdbook)
	mdbook serve docs

# Use the mdBook version of the docs workflow
MDBOOK_VERSION := $(shell sed -n 's/^ *MDBOOK_VERSION: *//p' .github/workflows/docs.yml)
MDBOOK_URL := https://github.com/rust-lang/mdBook/releases/download/$(MDBOOK_VERSION)/mdbook-$(MDBOOK_VERSION)
MDBOOK_DOCKER = docker run --rm --init -u "$$(id -u):$$(id -g)" $(1) alpine:3.22 \
	sh -c 'wget -qO- $(MDBOOK_URL)-$$(uname -m)-unknown-linux-musl.tar.gz | tar -xz -C /tmp && /tmp/mdbook $(2)'

docs-docker: ## docs in a container, needs only Docker
	$(call MDBOOK_DOCKER,-v "$(CURDIR)/docs":/docs,build /docs)

docs-serve-docker: ## docs-serve in a container, needs only Docker
	$(call MDBOOK_DOCKER,-p 3000:3000 -v "$(CURDIR)/docs":/docs:ro,serve /docs -d /tmp/book -n 0.0.0.0 -p 3000)
