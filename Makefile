# onda — developer Makefile
#
# Thin wrapper over cargo + `cargo xtask` (the source of truth for CI, bench,
# install, and bundle) plus the WASM reference-plugin build. Run `make` or
# `make help` for the target list.
#
# Common overrides:
#   make run FILE=README.md          # open a file
#   make run RUN_ARGS="doctor"       # pass raw args to the binary
#   make test CRATE=onda-core        # test a single crate
#   make CARGO="cargo +nightly" fmt  # use a different toolchain

CARGO        ?= cargo
FILE         ?=
RUN_ARGS     ?=
CRATE        ?=

# WASM reference plugins (excluded from the host workspace; built separately).
WASM_TARGET  := wasm32-wasip2
PLUGINS      := git-blame-inline todo-highlighter http-client hostile-test

# Where `cargo xtask install` places things (for `make uninstall`).
PREFIX       ?= $(HOME)/.local
BIN_DIR      := $(PREFIX)/bin
SHARE_DIR    := $(PREFIX)/share/onda

.DEFAULT_GOAL := help

# ── Help ─────────────────────────────────────────────────────────────────────
.PHONY: help
help: ## Show this help
	@echo "onda — make targets:"
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "} {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

# ── Develop ──────────────────────────────────────────────────────────────────
.PHONY: build release check run
build: ## Debug build the whole workspace
	$(CARGO) build --workspace

release: ## Release build the whole workspace
	$(CARGO) build --workspace --release

check: ## Fast type-check (no codegen), all targets
	$(CARGO) check --workspace --all-targets

run: ## Run the editor: make run FILE=path | RUN_ARGS="doctor"
	$(CARGO) run -p onda -- $(FILE) $(RUN_ARGS)

# ── Test & quality ───────────────────────────────────────────────────────────
.PHONY: test fmt fmt-check lint verify ci
test: ## Run tests (whole workspace, or CRATE=<name>)
ifeq ($(strip $(CRATE)),)
	$(CARGO) test --workspace
else
	$(CARGO) test -p $(CRATE)
endif

fmt: ## Format all code
	$(CARGO) fmt --all

fmt-check: ## Check formatting (no writes)
	$(CARGO) fmt --all --check

lint: ## Clippy with warnings denied (the CI gate)
	$(CARGO) clippy --workspace --all-targets -- -D warnings

verify: fmt-check lint test ## Local pre-commit gate: fmt + clippy + tests

ci: ## Full CI task (fmt check + clippy + tests + deny) via xtask
	$(CARGO) run -p xtask -- ci

# ── Benchmarks ───────────────────────────────────────────────────────────────
.PHONY: bench bench-check bench-compare fixtures
bench: ## Run benchmarks and print results
	$(CARGO) run -p xtask -- bench

bench-check: ## Check benchmarks against bench/baseline.json (fails on regression)
	$(CARGO) run -p xtask -- bench --check

bench-compare: ## Compare onda vs nvim/helix; write BENCH_REPORT.md
	$(CARGO) run -p xtask -- bench-compare

fixtures: ## Generate synthetic bench/test fixtures
	$(CARGO) run -p xtask -- gen-fixtures

# ── Install / deploy ─────────────────────────────────────────────────────────
.PHONY: install uninstall bundle doctor
install: ## Build release + install binary + runtime to ~/.local (via xtask)
	$(CARGO) run -p xtask -- install

uninstall: ## Remove the installed binary and runtime from ~/.local
	rm -f  $(BIN_DIR)/onda
	rm -rf $(SHARE_DIR)
	@echo "Removed $(BIN_DIR)/onda and $(SHARE_DIR)"

bundle: ## Assemble dist/ (binary + runtime) for packaging
	$(CARGO) run -p xtask -- bundle

doctor: ## Run environment diagnostics
	$(CARGO) run -p onda -- doctor

# ── WASM plugins ─────────────────────────────────────────────────────────────
.PHONY: wasm-target plugins plugins-clean
wasm-target: ## Add the wasm32-wasip2 rustup target (one-time)
	rustup target add $(WASM_TARGET)

plugins: ## Build all reference plugins to wasm32-wasip2 (release)
	@for p in $(PLUGINS); do \
		echo ">> building plugin: $$p"; \
		( cd plugins/$$p && $(CARGO) build --release --target $(WASM_TARGET) ) || exit 1; \
	done

plugins-clean: ## Clean reference-plugin build artifacts
	@for p in $(PLUGINS); do \
		( cd plugins/$$p && $(CARGO) clean ) || true; \
	done

# ── Housekeeping ─────────────────────────────────────────────────────────────
.PHONY: clean distclean
clean: ## Remove host build artifacts (target/)
	$(CARGO) clean

distclean: clean plugins-clean ## Clean everything (host + plugins + dist/)
	rm -rf dist
