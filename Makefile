SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c

DEV_DATA_DIR := $(CURDIR)/.dev-data
DEV_SOCKET := $(DEV_DATA_DIR)/run/synctv.sock
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

.PHONY: dev-up dev-stop dev-down dev-reset dev-data-reset dev-serve

dev-up:
	docker compose -f docker-compose.dev.yml up -d postgres redis

dev-stop:
	@pids="$$(pgrep -f 'target/debug/synctv serve|cargo run -p synctv -- serve' || true)"; \
	if [ -n "$$pids" ]; then \
		kill $$pids; \
	fi

dev-down: dev-stop
	docker compose -f docker-compose.dev.yml down -v

dev-data-reset:
	rm -rf "$(DEV_DATA_DIR)"
	mkdir -p "$(DEV_DATA_DIR)/run"

dev-reset: dev-down dev-data-reset

dev-serve: dev-up
	mkdir -p "$(DEV_DATA_DIR)/run"
	set -a; \
	if [ -f .env.synctv ]; then source .env.synctv; fi; \
	set +a; \
	export SYNCTV_DATA_DIR="$(DEV_DATA_DIR)"; \
	export SYNCTV_DATABASE_URL="$(DEV_DATABASE_URL)"; \
	export SYNCTV_REDIS_URL="$(DEV_REDIS_URL)"; \
	export SYNCTV_SERVER_HOST=0.0.0.0; \
	export SYNCTV_SERVER_PORT=8080; \
	export SYNCTV_SERVER_CORS_ALLOWED_ORIGINS='$(DEV_CORS_ORIGINS)'; \
	export SYNCTV_JWT_SECRET="$(DEV_JWT_SECRET)"; \
	export SYNCTV_CLUSTER_SECRET="$(DEV_CLUSTER_SECRET)"; \
	export SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY="$(DEV_CREDENTIAL_KEY)"; \
	export SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET="$(DEV_OPAQUE_SECRET)"; \
	export SYNCTV_SECURITY_SSRF_ENABLED=false; \
	export SYNCTV_SECURITY_SSRF_ALLOW_PRIVATE_NETWORK_TARGETS=false; \
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
	export SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH="$(DEV_SOCKET)"; \
	cargo run -p synctv -- serve
