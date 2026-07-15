SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

.DEFAULT_GOAL := help

COMPOSE ?= docker compose
CARGO := cargo +nightly
DEV_COMPOSE_FILE ?= docker-compose.dev.yml
DEV_PROJECT ?= synctv-dev
DEV_BASE_SERVICES ?= postgres redis
DEV_OPTIONAL_SERVICES ?= rustfs openlist emby jellyfin casdoor
DEV_STACK_SERVICES ?= $(DEV_BASE_SERVICES) $(DEV_OPTIONAL_SERVICES)
DEV_STACK_WAIT_SERVICES ?= $(DEV_STACK_SERVICES) rustfs-init openlist-init emby-init jellyfin-init
DEV_WAIT_TIMEOUT ?= 120
DEV_LOG_TAIL ?= 100
DEV_START_TIMEOUT ?= 120
DEV_JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 1)
DEV_FEATURES ?=
DEV_CARGO_FEATURE_ARGS := $(if $(strip $(DEV_FEATURES)),--features "$(DEV_FEATURES)",)
RELEASE_FEATURES ?=
RELEASE_CARGO_FEATURE_ARGS := $(if $(strip $(RELEASE_FEATURES)),--features "$(RELEASE_FEATURES)",)
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

.PHONY: help dev-check dev-env dev-up dev-stack dev-build release-build dev-serve dev-start dev-stop dev-down dev-clean dev-reset dev-data-reset dev-logs dev-ps dev-status dev-wait dev-shell dev-migrate dev-dropdb dev-db dev-redis dev-open dev-smoke fmt check check-all-targets sqlx-prepare nextest clippy

help: ## Show development targets.
	@awk 'BEGIN {FS = ":.*##"; printf "SyncTV development targets:\n"} /^[a-zA-Z0-9_.-]+:.*##/ {printf "  %-18s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

dev-check: ## Check required local tools.
	@command -v docker >/dev/null
	@$(COMPOSE) version >/dev/null
	@command -v rustup >/dev/null
	@$(CARGO) --version >/dev/null
	@printf "Docker, Docker Compose, and Cargo nightly are available.\n"

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
	SQLX_OFFLINE=true $(CARGO) build -p synctv --bin synctv $(DEV_CARGO_FEATURE_ARGS)

release-build: ## Build the optimized SyncTV release binary.
	SQLX_OFFLINE=true $(CARGO) build --release -p synctv --bin synctv $(RELEASE_CARGO_FEATURE_ARGS)

dev-serve: dev-up ## Run SyncTV locally with development defaults.
	mkdir -p "$(DEV_DATA_DIR)/run"
	$(DEV_ENV_EXPORTS); \
	SQLX_OFFLINE=true $(CARGO) run -p synctv --bin synctv $(DEV_CARGO_FEATURE_ARGS) -- serve

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

sqlx-prepare: dev-migrate ## Refresh SQLx offline metadata in .sqlx.
	DATABASE_URL="$(DEV_DATABASE_URL)" $(CARGO) sqlx prepare --workspace -- --all-targets

fmt: ## Format all Rust code.
	$(CARGO) fmt --all

check: ## Check workspace library and binary targets.
	SQLX_OFFLINE=true $(CARGO) check -j "$(DEV_JOBS)" --workspace

check-all-targets: ## Check all workspace targets, including tests, benches, and examples.
	SQLX_OFFLINE=true $(CARGO) check -j "$(DEV_JOBS)" --workspace --all-targets

nextest: ## Run the full workspace nextest suite, including ignored tests.
	SQLX_OFFLINE=true $(CARGO) nextest run --workspace --run-ignored all -j "$(DEV_JOBS)" --nff --status-level fail

clippy: ## Apply Clippy fixes, then require a clean workspace lint pass.
	SQLX_OFFLINE=true $(CARGO) clippy -j "$(DEV_JOBS)" --workspace --all-targets --fix --allow-dirty
	SQLX_OFFLINE=true $(CARGO) clippy -j "$(DEV_JOBS)" --workspace --all-targets

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
