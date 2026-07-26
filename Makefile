SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c

.DEFAULT_GOAL := help

COMPOSE ?= docker compose
PROD_COMPOSE_FILE ?= docker-compose.yml
COMPOSE_PROD := $(COMPOSE) -f $(PROD_COMPOSE_FILE)
COMPOSE_ENV_FILES := .env.postgres .env.redis .env.synctv
RUST_TOOLCHAIN ?= nightly
CARGO ?= cargo +$(RUST_TOOLCHAIN)
CROSS ?= cargo cross +$(RUST_TOOLCHAIN)
CARGO_LOCKED ?= --locked
CARGO_WORKSPACE_ARGS ?= --workspace
CARGO_ALL_TARGETS_ARGS ?= --all-targets
DEV_COMPOSE_FILE ?= docker-compose.dev.yml
DEV_PROJECT ?= synctv-dev
DEV_BASE_SERVICES ?= postgres redis
DEV_OPTIONAL_SERVICES ?= rustfs openlist emby jellyfin casdoor
DEV_STACK_SERVICES ?= $(DEV_BASE_SERVICES) $(DEV_OPTIONAL_SERVICES)
DEV_STACK_WAIT_SERVICES ?= $(DEV_STACK_SERVICES) rustfs-init openlist-init emby-init jellyfin-init
DEV_WAIT_TIMEOUT ?= 120
DEV_LOG_TAIL ?= 100
DEV_START_TIMEOUT ?= 120
CPU_COUNT ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 1)
DEV_JOBS ?= $(CPU_COUNT)
CARGO_JOBS_ARGS ?= -j "$(DEV_JOBS)"
CARGO_BUILD_ARGS ?= $(CARGO_JOBS_ARGS) $(CARGO_LOCKED)
CARGO_WORKSPACE_BUILD_ARGS ?= $(CARGO_BUILD_ARGS) $(CARGO_WORKSPACE_ARGS)
CARGO_WORKSPACE_ALL_TARGETS_BUILD_ARGS ?= $(CARGO_WORKSPACE_BUILD_ARGS) $(CARGO_ALL_TARGETS_ARGS)
NEXTEST_STATUS_ARGS ?= --status-level slow --final-status-level slow
DEV_FEATURES ?=
DEV_CARGO_FEATURE_ARGS := $(if $(strip $(DEV_FEATURES)),--no-default-features --features "$(DEV_FEATURES)",)
RELEASE_FEATURES ?=
RELEASE_CARGO_FEATURE_ARGS := $(if $(strip $(RELEASE_FEATURES)),--features "$(RELEASE_FEATURES)",)
FEATURE_CHECK_ARGS ?=
CROSS_PACKAGE ?= synctv
CROSS_FEATURE_ARGS ?=
CROSS_LINUX_TARGET ?= x86_64-unknown-linux-gnu
CROSS_WINDOWS_TARGET ?= x86_64-pc-windows-gnu
CROSS_DARWIN_TARGET ?= aarch64-apple-darwin
CROSS_CHECK_ARGS ?= $(CARGO_BUILD_ARGS) -p $(CROSS_PACKAGE) $(CARGO_ALL_TARGETS_ARGS) $(CROSS_FEATURE_ARGS)
TLS_RING_KEY_CRATES := synctv-core synctv-api synctv-cluster synctv-realtime synctv-livestream synctv-media-providers synctv-proxy
TLS_RING_KEY_PACKAGE_ARGS := $(foreach crate,$(TLS_RING_KEY_CRATES),-p $(crate))
TLS_RING_KEY_FEATURE_ARGS := $(foreach crate,$(TLS_RING_KEY_CRATES),$(crate)/tls-ring $(crate)/tls-webpki-roots)
DEV_SSRF_ENABLED ?= false
DEV_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS ?= false

DEV_DATA_DIR := $(CURDIR)/.dev-data
DEV_SOCKET := $(DEV_DATA_DIR)/run/synctv.sock
DEV_PID := $(DEV_DATA_DIR)/run/synctv.pid
DEV_LOG := $(DEV_DATA_DIR)/run/synctv.log
DEV_BIN ?= $(CURDIR)/target/debug/synctv
DEV_DATABASE_URL := postgresql://synctv:synctv@127.0.0.1:5432/synctv
DEV_REDIS_URL := redis://127.0.0.1:6379
DEV_ROOT_USERNAME := root
DEV_ROOT_PASSWORD := LocalDevRootPass2026!
DEV_JWT_SECRET := local-compose-jwt-secret-not-for-production-2026
DEV_CLUSTER_SECRET := local-compose-cluster-secret-2026
DEV_CREDENTIAL_KEY := 222102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
DEV_OPAQUE_SECRET := local-compose-opaque-server-setup-secret-not-for-production-2026
DEV_AUTH_MAX_REQUESTS ?= 10000
DEV_AUTH_WINDOW_SECONDS ?= 1
DEV_CORS_ORIGINS := ["http://localhost:3000","http://127.0.0.1:3000","http://localhost:5173","http://127.0.0.1:5173","http://localhost:8080","http://127.0.0.1:8080"]
DEV_FILE_STORAGE_BACKENDS := {"database":{"type":"database"}}

COMPOSE_DEV := $(COMPOSE) -p $(DEV_PROJECT) -f $(DEV_COMPOSE_FILE)
COMPOSE_DEV_PROFILES := COMPOSE_PROFILES=media,storage,auth $(COMPOSE_DEV)

define DEV_ENV_EXPORTS
set -a; \
if [ -f .env.synctv ]; then source .env.synctv; fi; \
set +a; \
export SYNCTV_DATA_DIR="$(DEV_DATA_DIR)"; \
export SYNCTV_DATABASE_URL="$(DEV_DATABASE_URL)"; \
export SYNCTV_REDIS_URL="$(DEV_REDIS_URL)"; \
export SYNCTV_LOGGING_LEVEL="$${SYNCTV_LOGGING_LEVEL:-debug}"; \
export SYNCTV_SERVER_HOST=0.0.0.0; \
export SYNCTV_SERVER_PORT=8080; \
export SYNCTV_SERVER_CORS_ALLOWED_ORIGINS='$(DEV_CORS_ORIGINS)'; \
export SYNCTV_JWT_SECRET="$(DEV_JWT_SECRET)"; \
export SYNCTV_CLUSTER_SECRET="$(DEV_CLUSTER_SECRET)"; \
export SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY="$(DEV_CREDENTIAL_KEY)"; \
export SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET="$(DEV_OPAQUE_SECRET)"; \
export SYNCTV_REQUEST_RATE_LIMITS_AUTH_MAX_REQUESTS="$(DEV_AUTH_MAX_REQUESTS)"; \
export SYNCTV_REQUEST_RATE_LIMITS_AUTH_WINDOW_SECONDS="$(DEV_AUTH_WINDOW_SECONDS)"; \
export SYNCTV_SECURITY_SSRF_ENABLED="$(DEV_SSRF_ENABLED)"; \
export SYNCTV_SECURITY_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS="$(DEV_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS)"; \
export SYNCTV_FILE_STORAGE_DEFAULT_BACKEND=database; \
export SYNCTV_FILE_STORAGE_CHAT_ATTACHMENTS_BACKEND=database; \
export SYNCTV_FILE_STORAGE_USER_AVATARS_BACKEND=database; \
export SYNCTV_FILE_STORAGE_MEDIA_COVERS_BACKEND=database; \
export SYNCTV_FILE_STORAGE_ROOM_COVERS_BACKEND=database; \
export SYNCTV_FILE_STORAGE_PLAYLIST_COVERS_BACKEND=database; \
export SYNCTV_FILE_STORAGE_BACKENDS='$(DEV_FILE_STORAGE_BACKENDS)'; \
export SYNCTV_BOOTSTRAP_CREATE_ROOT_USER=true; \
export SYNCTV_BOOTSTRAP_ROOT_USERNAME="$(DEV_ROOT_USERNAME)"; \
export SYNCTV_BOOTSTRAP_ROOT_PASSWORD="$(DEV_ROOT_PASSWORD)"; \
export SYNCTV_WEBRTC_ENABLE_BUILTIN_STUN=false; \
export SYNCTV_MANAGEMENT_ENABLED=true; \
export SYNCTV_MANAGEMENT_TRANSPORT=unix; \
export SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH="$(DEV_SOCKET)"
endef

.PHONY: help clean compose-init compose-config compose-pull compose-up compose-down compose-logs compose-ps dev-check dev-env dev-up dev-stack dev-build release-build dev-serve dev-start dev-stop dev-down dev-clean dev-reset dev-data-reset dev-logs dev-ps dev-status dev-wait dev-shell dev-migrate dev-dropdb dev-db dev-redis dev-open dev-smoke fmt fmt-check check check-all-targets build-workspace proto-freshness feature-check feature-check-key-crates-tls-ring-webpki sqlx-prepare nextest nextest-default nextest-ignored doc-test clippy clippy-check install-cargo-audit audit audit-advisories install-cargo-deny deny-check deny-advisories deny-licenses deny-bans deny-sources install-cargo-udeps udeps cargo-workspace-version set-release-version validate-helm require-cross install-cross cross-linux-check cross-windows-check cross-darwin-check cross-linux-clippy cross-windows-clippy cross-darwin-clippy

help: ## Show available targets.
	@awk 'BEGIN {FS = ":.*##"; printf "SyncTV targets:\n"} /^[a-zA-Z0-9_.-]+:.*##/ {printf "  %-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

compose-init: ## Create production Compose env files with generated secrets.
	@command -v openssl >/dev/null || { printf "openssl is required.\n" >&2; exit 1; }
	@for file in $(COMPOSE_ENV_FILES); do \
		if [ -e "$$file" ]; then \
			printf "%s already exists; refusing to overwrite it.\n" "$$file" >&2; \
			exit 1; \
		fi; \
	done
	@trap 'rm -f $(COMPOSE_ENV_FILES) .env.postgres.bak .env.redis.bak .env.synctv.bak' ERR; \
	umask 077; \
	cp .env.postgres.example .env.postgres; \
	cp .env.redis.example .env.redis; \
	cp .env.synctv.example .env.synctv; \
	set_env() { \
		local file="$$1" key="$$2" value="$$3"; \
		sed -i.bak "s|^$${key}=.*|$${key}=$${value}|" "$$file"; \
	}; \
	postgres_password="$$(openssl rand -hex 32)"; \
	redis_password="$$(openssl rand -hex 32)"; \
	set_env .env.postgres POSTGRES_PASSWORD "$$postgres_password"; \
	set_env .env.redis REDIS_PASSWORD "$$redis_password"; \
	set_env .env.synctv SYNCTV_DATABASE_URL "postgresql://synctv:$${postgres_password}@postgres:5432/synctv"; \
	set_env .env.synctv SYNCTV_REDIS_URL "redis://:$${redis_password}@redis:6379"; \
	set_env .env.synctv SYNCTV_JWT_SECRET "$$(openssl rand -base64 32)"; \
	set_env .env.synctv SYNCTV_CLUSTER_SECRET "$$(openssl rand -hex 32)"; \
	set_env .env.synctv SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY "$$(openssl rand -hex 32)"; \
	set_env .env.synctv SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET "$$(openssl rand -base64 48)"; \
	rm -f .env.postgres.bak .env.redis.bak .env.synctv.bak; \
	chmod 600 $(COMPOSE_ENV_FILES); \
	trap - ERR
	@printf "Created production Compose env files. Set SYNCTV_BOOTSTRAP_ROOT_PASSWORD in .env.synctv, then run 'make compose-up'.\n"

compose-config: ## Render and validate the production Compose configuration.
	$(COMPOSE_PROD) config

compose-pull: ## Pull production Compose images.
	$(COMPOSE_PROD) pull

compose-up: ## Validate and start the production Compose stack.
	$(COMPOSE_PROD) config --quiet
	$(COMPOSE_PROD) up -d

compose-down: ## Stop the production Compose stack and keep volumes.
	$(COMPOSE_PROD) down --remove-orphans

compose-logs: ## Follow production Compose logs. Set SERVICE=name to focus.
	$(COMPOSE_PROD) logs -f --tail=100 $(SERVICE)

compose-ps: ## Show production Compose service status.
	$(COMPOSE_PROD) ps

dev-check: ## Check required local tools.
	@command -v docker >/dev/null
	@$(COMPOSE) version >/dev/null
	@command -v rustup >/dev/null
	@$(CARGO) --version >/dev/null
	@printf "Docker, Docker Compose, and Cargo %s are available.\n" "$(RUST_TOOLCHAIN)"

dev-env: ## Print local service URLs and credentials.
	@printf "SyncTV:    http://127.0.0.1:8080  root / %s\n" "$(DEV_ROOT_PASSWORD)"
	@printf "Postgres:  %s\n" "$(DEV_DATABASE_URL)"
	@printf "Redis:     %s\n" "$(DEV_REDIS_URL)"
	@printf "OpenList:  http://127.0.0.1:5244  admin / synctv-openlist\n"
	@printf "Emby:      http://127.0.0.1:8096  MyEmbyUser / synctv-emby\n"
	@printf "Jellyfin:  http://127.0.0.1:8097  root / synctv-jellyfin\n"
	@printf "RustFS:    http://127.0.0.1:9000  rustfsadmin / rustfsadmin\n"
	@printf "RustFS UI: http://127.0.0.1:9001\n"
	@printf "Casdoor:   http://127.0.0.1:8000  admin / 123\n"

dev-up: dev-check ## Start core development dependencies: PostgreSQL and Redis.
	$(COMPOSE_DEV) up -d $(DEV_BASE_SERVICES)
	@$(MAKE) dev-wait SERVICES="$(DEV_BASE_SERVICES)"

dev-stack: dev-up ## Start OpenList, Emby, Jellyfin, RustFS, and Casdoor after core dependencies.
	@$(COMPOSE_DEV) exec -T postgres psql -U synctv -d synctv -tc "SELECT 1 FROM pg_database WHERE datname = 'casdoor'" | grep -q 1 || \
		$(COMPOSE_DEV) exec -T postgres createdb -U synctv casdoor
	$(COMPOSE_DEV_PROFILES) up -d $(DEV_OPTIONAL_SERVICES) rustfs-init openlist-init emby-init jellyfin-init
	@$(MAKE) dev-wait SERVICES="$(DEV_STACK_WAIT_SERVICES)"
	@$(MAKE) dev-env

dev-build: ## Build the local SyncTV binary used by background dev commands.
	SQLX_OFFLINE=true $(CARGO) build $(CARGO_BUILD_ARGS) -p synctv --bin synctv $(DEV_CARGO_FEATURE_ARGS)

release-build: ## Build the optimized SyncTV release binary.
	SQLX_OFFLINE=true $(CARGO) build $(CARGO_BUILD_ARGS) --release -p synctv --bin synctv $(RELEASE_CARGO_FEATURE_ARGS)

dev-serve: dev-up ## Run SyncTV locally with development defaults.
	mkdir -p "$(DEV_DATA_DIR)/run"
	$(DEV_ENV_EXPORTS); \
	SQLX_OFFLINE=true $(CARGO) run $(CARGO_BUILD_ARGS) -p synctv --bin synctv $(DEV_CARGO_FEATURE_ARGS) -- serve

dev-start: dev-up dev-build ## Start SyncTV in the background with development defaults.
	@mkdir -p "$(DEV_DATA_DIR)/run"
	@if [ -f "$(DEV_PID)" ] && kill -0 "$$(cat "$(DEV_PID)")" 2>/dev/null; then \
		printf "SyncTV already running with pid %s.\n" "$$(cat "$(DEV_PID)")"; \
		exit 0; \
	fi
	@if [ -S "$(DEV_SOCKET)" ] && "$(DEV_BIN)" --endpoint "unix://$(DEV_SOCKET)" system stats --output json >/dev/null 2>&1; then \
		printf "SyncTV already responding on %s.\n" "$(DEV_SOCKET)"; \
		exit 0; \
	fi
	@rm -f "$(DEV_PID)" "$(DEV_SOCKET)"
	@$(DEV_ENV_EXPORTS); \
	nohup "$(DEV_BIN)" serve >"$(DEV_LOG)" 2>&1 < /dev/null & \
	pid="$$!"; \
	printf "%s\n" "$$pid" >"$(DEV_PID)"; \
	printf "Started SyncTV pid %s. Logs: %s\n" "$$pid" "$(DEV_LOG)"; \
	deadline=$$((SECONDS + $(DEV_START_TIMEOUT))); \
	until [ -S "$(DEV_SOCKET)" ] && "$(DEV_BIN)" --endpoint "unix://$(DEV_SOCKET)" system stats --output json >/dev/null 2>&1 && curl -fsS http://127.0.0.1:8080/health/ready >/dev/null; do \
		if ! kill -0 "$$pid" 2>/dev/null; then \
			printf "SyncTV exited during startup. Last log lines:\n"; \
			tail -n 80 "$(DEV_LOG)" || true; \
			exit 1; \
		fi; \
		if [ "$$SECONDS" -ge "$$deadline" ]; then \
			printf "Timed out waiting for SyncTV. Last log lines:\n"; \
			tail -n 80 "$(DEV_LOG)" || true; \
			exit 1; \
		fi; \
		sleep 2; \
	done; \
	printf "SyncTV ready at http://127.0.0.1:8080.\n"

dev-stop: ## Stop locally running SyncTV processes started by dev-serve/dev-start.
	@if [ -S "$(DEV_SOCKET)" ] && [ -x "$(DEV_BIN)" ]; then \
		"$(DEV_BIN)" --endpoint "unix://$(DEV_SOCKET)" stop >/dev/null 2>&1 || true; \
	fi
	@if [ -f "$(DEV_PID)" ]; then \
		pid="$$(cat "$(DEV_PID)")"; \
		if [ -n "$$pid" ] && kill -0 "$$pid" 2>/dev/null; then \
			for _ in $$(seq 1 20); do \
				kill -0 "$$pid" 2>/dev/null || break; \
				sleep 0.5; \
			done; \
			if kill -0 "$$pid" 2>/dev/null; then \
				kill "$$pid" 2>/dev/null || true; \
			fi; \
			printf "Stopped local SyncTV process: %s\n" "$$pid"; \
		fi; \
	fi
	@pids="$$(pgrep -f '[/]synctv serve|[c]argo (\+nightly )?run -p synctv .* serve' || true)"; \
	if [ -n "$$pids" ]; then \
		kill $$pids; \
		printf "Stopped local SyncTV process(es): %s\n" "$$pids"; \
	elif [ ! -f "$(DEV_PID)" ]; then \
		printf "No local SyncTV process found.\n"; \
	fi; \
	rm -f "$(DEV_PID)" "$(DEV_SOCKET)"

dev-down: dev-stop ## Stop development containers and keep volumes.
	$(COMPOSE_DEV_PROFILES) down --remove-orphans

dev-clean: dev-stop ## Stop development containers and remove Compose volumes.
	$(COMPOSE_DEV_PROFILES) down -v --remove-orphans

dev-data-reset: ## Remove local .dev-data used by dev-serve.
	rm -rf "$(DEV_DATA_DIR)"
	mkdir -p "$(DEV_DATA_DIR)/run"

dev-reset: dev-clean dev-data-reset ## Remove containers, volumes, and local dev data.

dev-logs: ## Tail development dependency logs. Set SERVICE=name to focus.
	$(COMPOSE_DEV_PROFILES) logs -f --tail=$(DEV_LOG_TAIL) $(SERVICE)

dev-ps: ## Show development container status.
	$(COMPOSE_DEV_PROFILES) ps

dev-status: dev-ps ## Show container status and local environment.
	@$(MAKE) dev-env

dev-wait: ## Wait for Compose services to become healthy/running. Set SERVICES="postgres redis".
	@services="$${SERVICES:-$(DEV_BASE_SERVICES)}"; \
	deadline=$$((SECONDS + $(DEV_WAIT_TIMEOUT))); \
	for service in $$services; do \
		printf "Waiting for %s" "$$service"; \
		while true; do \
			id="$$( $(COMPOSE_DEV_PROFILES) ps -a -q "$$service" 2>/dev/null || true )"; \
			if [ -n "$$id" ]; then \
				health="$$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$$id" 2>/dev/null || true)"; \
				exit_code="$$(docker inspect -f '{{.State.ExitCode}}' "$$id" 2>/dev/null || echo 1)"; \
				case "$$health" in \
					healthy|running) printf " %s\n" "$$health"; break ;; \
					exited) \
						if [ "$$exit_code" = "0" ]; then \
							printf " completed\n"; \
							break; \
						fi; \
						printf "\n%s exited before becoming ready.\n" "$$service"; \
						exit 1; \
						;; \
					dead) printf "\n%s exited before becoming ready.\n" "$$service"; exit 1 ;; \
				esac; \
			fi; \
			if [ "$$SECONDS" -ge "$$deadline" ]; then \
				printf "\nTimed out waiting for %s.\n" "$$service"; \
				$(COMPOSE_DEV_PROFILES) ps "$$service"; \
				exit 1; \
			fi; \
			printf "."; \
			sleep 2; \
		done; \
	done

dev-shell: dev-up ## Open a shell with SyncTV development environment variables.
	mkdir -p "$(DEV_DATA_DIR)/run"
	env \
		SYNCTV_DATA_DIR="$(DEV_DATA_DIR)" \
		SYNCTV_DATABASE_URL="$(DEV_DATABASE_URL)" \
		SYNCTV_REDIS_URL="$(DEV_REDIS_URL)" \
		SYNCTV_JWT_SECRET="$(DEV_JWT_SECRET)" \
		SYNCTV_CLUSTER_SECRET="$(DEV_CLUSTER_SECRET)" \
		SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY="$(DEV_CREDENTIAL_KEY)" \
		SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET="$(DEV_OPAQUE_SECRET)" \
		SYNCTV_BOOTSTRAP_CREATE_ROOT_USER=true \
		SYNCTV_BOOTSTRAP_ROOT_USERNAME="$(DEV_ROOT_USERNAME)" \
		SYNCTV_BOOTSTRAP_ROOT_PASSWORD="$(DEV_ROOT_PASSWORD)" \
		$${SHELL:-/bin/bash}

dev-migrate: dev-up ## Run database migrations against the local PostgreSQL container.
	DATABASE_URL="$(DEV_DATABASE_URL)" $(CARGO) sqlx migrate run

dev-dropdb: dev-up ## Drop and recreate the local development PostgreSQL database.
	$(COMPOSE_DEV) exec -T postgres sh -lc 'dropdb -U synctv --if-exists synctv && createdb -U synctv synctv'

clean: ## Remove Rust workspace build artifacts.
	$(CARGO) clean

sqlx-prepare: dev-migrate ## Refresh SQLx offline metadata in .sqlx.
	DATABASE_URL="$(DEV_DATABASE_URL)" $(CARGO) sqlx prepare --workspace -- --all-targets

fmt: ## Format all Rust code.
	$(CARGO) fmt --all

fmt-check: ## Check Rust formatting without modifying files.
	$(CARGO) fmt --all -- --check

check: ## Check workspace library and binary targets.
	SQLX_OFFLINE=true $(CARGO) check $(CARGO_WORKSPACE_BUILD_ARGS)

check-all-targets: ## Check all workspace targets, including tests, benches, and examples.
	SQLX_OFFLINE=true $(CARGO) check $(CARGO_WORKSPACE_ALL_TARGETS_BUILD_ARGS)

build-workspace: ## Build the locked workspace dependency graph.
	SQLX_OFFLINE=true $(CARGO) build $(CARGO_WORKSPACE_BUILD_ARGS) --verbose

proto-freshness: ## Regenerate protobuf artifacts and require a clean generated diff.
	SYNCTV_REGEN_PROTO=1 SQLX_OFFLINE=true $(CARGO) check $(CARGO_BUILD_ARGS) -p synctv-proto -p synctv-media-providers -p synctv-cluster -p synctv-livestream -p synctv-realtime -p synctv-proxy
	git diff --exit-code -- synctv-proto/src synctv-media-providers/src/proto

feature-check: ## Check synctv with FEATURE_CHECK_ARGS supplied by CI or the caller.
	SQLX_OFFLINE=true $(CARGO) check $(CARGO_BUILD_ARGS) -p synctv $(FEATURE_CHECK_ARGS)

feature-check-key-crates-tls-ring-webpki: ## Check key crates with the ring/webpki TLS feature pair.
	SQLX_OFFLINE=true $(CARGO) check $(CARGO_BUILD_ARGS) $(TLS_RING_KEY_PACKAGE_ARGS) --no-default-features --features "$(TLS_RING_KEY_FEATURE_ARGS)"

nextest: ## Run the full workspace nextest suite, including ignored tests.
	SQLX_OFFLINE=true $(CARGO) nextest run $(CARGO_WORKSPACE_BUILD_ARGS) --run-ignored all --nff $(NEXTEST_STATUS_ARGS)

nextest-default: ## Run non-ignored workspace tests with nextest.
	SQLX_OFFLINE=true $(CARGO) nextest run $(CARGO_WORKSPACE_BUILD_ARGS) --run-ignored default --nff $(NEXTEST_STATUS_ARGS)

nextest-ignored: ## Run ignored workspace tests with nextest.
	SQLX_OFFLINE=true $(CARGO) nextest run $(CARGO_WORKSPACE_BUILD_ARGS) --run-ignored only --nff $(NEXTEST_STATUS_ARGS)

doc-test: ## Run locked workspace documentation tests.
	SQLX_OFFLINE=true $(CARGO) test $(CARGO_WORKSPACE_BUILD_ARGS) --doc

clippy: ## Apply Clippy fixes, then require a clean workspace lint pass.
	SQLX_OFFLINE=true $(CARGO) clippy $(CARGO_WORKSPACE_ALL_TARGETS_BUILD_ARGS) --fix --allow-dirty

clippy-check: ## Run locked workspace Clippy checks without modifying files.
	SQLX_OFFLINE=true $(CARGO) clippy $(CARGO_WORKSPACE_ALL_TARGETS_BUILD_ARGS)

install-cargo-audit: ## Install cargo-audit for CI security checks.
	$(CARGO) install cargo-audit $(CARGO_LOCKED)

audit: ## Fail on RustSec vulnerabilities.
	$(CARGO) audit

audit-advisories: ## Print the complete RustSec advisory report.
	$(CARGO) audit

install-cargo-deny: ## Install cargo-deny for dependency policy checks.
	$(CARGO) install cargo-deny $(CARGO_LOCKED)

deny-check: ## Run all cargo-deny policy checks.
	$(CARGO) deny check

deny-advisories:
	$(CARGO) deny check advisories

deny-licenses:
	$(CARGO) deny check licenses

deny-bans:
	$(CARGO) deny check bans

deny-sources:
	$(CARGO) deny check sources

install-cargo-udeps: ## Install cargo-udeps for unused dependency checks.
	$(CARGO) install cargo-udeps $(CARGO_LOCKED)

udeps: ## Check all workspace targets for unused dependencies.
	SQLX_OFFLINE=true $(CARGO) udeps $(CARGO_WORKSPACE_ALL_TARGETS_BUILD_ARGS)

cargo-workspace-version: ## Print the Cargo workspace version.
	@$(CARGO) metadata --format-version 1 --no-deps $(CARGO_LOCKED) | node -e 'const fs = require("fs"); const meta = JSON.parse(fs.readFileSync(0, "utf8")); const id = meta.workspace_default_members[0]; process.stdout.write((meta.packages.find((pkg) => pkg.id === id) || meta.packages[0]).version);'

set-release-version: ## Synchronize release files. Set VERSION=x.y.z.
	@test -n "$(VERSION)" || { printf "VERSION is required.\n" >&2; exit 1; }
	RUSTUP_TOOLCHAIN="$(RUST_TOOLCHAIN)" scripts/set-release-version.sh "$(VERSION)"

validate-helm: ## Validate Helm charts and rendered SyncTV configuration.
	RUSTUP_TOOLCHAIN="$(RUST_TOOLCHAIN)" scripts/validate-helm.sh

require-cross:
	@command -v cargo-cross >/dev/null || { printf "cargo-cross is required; run 'make install-cross'.\n" >&2; exit 1; }

install-cross: ## Install cargo-cross using the configured Rust toolchain.
	$(CARGO) install cargo-cross $(CARGO_LOCKED)

cross-linux-check: require-cross ## Cross-check SyncTV for Linux.
	SQLX_OFFLINE=true $(CROSS) check $(CROSS_CHECK_ARGS) --target "$(CROSS_LINUX_TARGET)"

cross-windows-check: require-cross ## Cross-check SyncTV for Windows GNU.
	SQLX_OFFLINE=true $(CROSS) check $(CROSS_CHECK_ARGS) --target "$(CROSS_WINDOWS_TARGET)"

cross-darwin-check: require-cross ## Cross-check SyncTV for Darwin.
	SQLX_OFFLINE=true $(CROSS) check $(CROSS_CHECK_ARGS) --target "$(CROSS_DARWIN_TARGET)"

cross-linux-clippy: require-cross ## Run cross Clippy for Linux.
	SQLX_OFFLINE=true $(CROSS) clippy $(CROSS_CHECK_ARGS) --target "$(CROSS_LINUX_TARGET)"

cross-windows-clippy: require-cross ## Run cross Clippy for Windows GNU.
	SQLX_OFFLINE=true $(CROSS) clippy $(CROSS_CHECK_ARGS) --target "$(CROSS_WINDOWS_TARGET)"

cross-darwin-clippy: require-cross ## Run cross Clippy for Darwin.
	SQLX_OFFLINE=true $(CROSS) clippy $(CROSS_CHECK_ARGS) --target "$(CROSS_DARWIN_TARGET)"

dev-db: dev-up ## Open psql inside the PostgreSQL container.
	$(COMPOSE_DEV) exec postgres psql -U synctv -d synctv

dev-redis: dev-up ## Open redis-cli inside the Redis container.
	$(COMPOSE_DEV) exec redis redis-cli

dev-open: ## Open common local service URLs on macOS.
	@open http://127.0.0.1:8080 || true
	@open http://127.0.0.1:5244 || true
	@open http://127.0.0.1:9001 || true
	@open http://127.0.0.1:8000 || true

dev-smoke: ## Run real CLI/curl smoke tests against the local dev stack.
	scripts/dev-e2e-smoke.sh
