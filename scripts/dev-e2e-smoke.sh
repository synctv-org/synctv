#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

COMPOSE="${COMPOSE:-docker compose}"
DEV_COMPOSE_FILE="${DEV_COMPOSE_FILE:-docker-compose.dev.yml}"
DEV_PROJECT="${DEV_PROJECT:-synctv-dev}"
read -r -a COMPOSE_DEV <<<"$COMPOSE"
COMPOSE_DEV+=(-p "$DEV_PROJECT" -f "$DEV_COMPOSE_FILE")

DATA_DIR="$ROOT_DIR/.dev-data"
RUN_DIR="$DATA_DIR/run"
SMOKE_DIR="$DATA_DIR/provider-smoke"
HTTP_DIR="$SMOKE_DIR/http"
RESULTS_DIR="$SMOKE_DIR/results"
SOCK="unix://$RUN_DIR/synctv.sock"
BIN="${SYNCTV_BIN:-$ROOT_DIR/target/debug/synctv}"
BASE_URL="${SYNCTV_BASE_URL:-http://127.0.0.1:8080}"
STATIC_PORT="${DEV_STATIC_PORT:-18080}"
STATIC_URL="http://127.0.0.1:$STATIC_PORT"
RUN_ID="${SYNCTV_DEV_SMOKE_RUN_ID:-$(date +%Y%m%d%H%M%S)}"
SMOKE_USER="devuser_$RUN_ID"
SMOKE_ROOM="Smoke Room $RUN_ID"

HTTP_PID=""

log() {
  printf '\n==> %s\n' "$*"
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "${HTTP_PID:-}" ] && kill -0 "$HTTP_PID" 2>/dev/null; then
    kill "$HTTP_PID" 2>/dev/null || true
  fi
  make dev-stop DEV_BIN="$BIN" >/dev/null 2>&1 || true
}
trap cleanup EXIT

json_field() {
  jq -r "$1 // empty"
}

cli() {
  "$BIN" --endpoint "$SOCK" "$@"
}

cli_json() {
  "$BIN" --endpoint "$SOCK" "$@" --output json
}

wait_http() {
  local url="$1"
  local deadline=$((SECONDS + 120))
  until curl -fsS "$url" >/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      die "Timed out waiting for $url"
    fi
    sleep 2
  done
}

wait_socket() {
  local deadline=$((SECONDS + 120))
  until [ -S "$RUN_DIR/synctv.sock" ] && cli system stats --output json >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      die "Timed out waiting for SyncTV management socket"
    fi
    sleep 2
  done
}

ensure_bin() {
  if [ ! -x "$BIN" ]; then
    cargo build -p synctv --bin synctv
  fi
}

start_stack() {
  log "Starting dependency stack"
  make dev-stack
}

start_synctv() {
  log "Starting SyncTV host server"
  mkdir -p "$RUN_DIR" "$RESULTS_DIR"
  make dev-stop DEV_BIN="$BIN" >/dev/null 2>&1 || true
  make dev-start DEV_BIN="$BIN"
  wait_socket
  wait_http "$BASE_URL/health/ready"
}

make_media() {
  log "Generating sample media"
  mkdir -p "$HTTP_DIR" "$RESULTS_DIR"
  ffmpeg -y \
    -f lavfi -i testsrc=size=320x180:rate=25 \
    -f lavfi -i sine=frequency=1000:sample_rate=48000 \
    -t 4 -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac \
    "$HTTP_DIR/direct.mp4" >/dev/null 2>&1
  ffmpeg -y \
    -f lavfi -i testsrc=size=320x180:rate=25 \
    -f lavfi -i sine=frequency=880:sample_rate=48000 \
    -t 4 -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac \
    -f hls -hls_time 1 -hls_list_size 0 -hls_segment_filename "$HTTP_DIR/stream%d.ts" \
    "$HTTP_DIR/stream.m3u8" >/dev/null 2>&1
  ffmpeg -y \
    -f lavfi -i testsrc=size=320x180:rate=25 \
    -f lavfi -i sine=frequency=660:sample_rate=48000 \
    -t 8 -pix_fmt yuv420p -c:v libx264 -preset ultrafast -c:a aac \
    -f flv "$HTTP_DIR/live.flv" >/dev/null 2>&1

  python3 -m http.server "$STATIC_PORT" --bind 127.0.0.1 --directory "$HTTP_DIR" >"$RESULTS_DIR/static-http.log" 2>&1 &
  HTTP_PID=$!
  sleep 1
  if ! kill -0 "$HTTP_PID" 2>/dev/null; then
    cat "$RESULTS_DIR/static-http.log" >&2 || true
    die "Static HTTP server failed to start"
  fi
  wait_http "$STATIC_URL/direct.mp4"

  "${COMPOSE_DEV[@]}" cp "$HTTP_DIR/direct.mp4" openlist:/opt/openlist/dev-media/direct.mp4
  "${COMPOSE_DEV[@]}" cp "$HTTP_DIR/direct.mp4" emby:/mnt/share1/direct.mp4
  "${COMPOSE_DEV[@]}" cp "$HTTP_DIR/direct.mp4" jellyfin:/media/direct.mp4
}

prepare_media_servers() {
  log "Refreshing Emby and Jellyfin libraries"
  local em_auth em_token jf_auth jf_token
  em_auth="$(curl -fsS -X POST http://127.0.0.1:8096/Users/AuthenticateByName \
    -H 'Content-Type: application/json' \
    -H 'X-Emby-Authorization: MediaBrowser Client="SyncTVDev", Device="curl", DeviceId="smoke-emby", Version="1"' \
    -d '{"Username":"MyEmbyUser","Pw":"synctv-emby"}')"
  em_token="$(printf '%s' "$em_auth" | jq -r .AccessToken)"
  curl -fsS -X POST http://127.0.0.1:8096/Library/Refresh -H "X-Emby-Token: $em_token" >/dev/null

  jf_auth="$(curl -fsS -X POST http://127.0.0.1:8097/Users/AuthenticateByName \
    -H 'Content-Type: application/json' \
    -H 'Authorization: MediaBrowser Client="SyncTVDev", Device="curl", DeviceId="smoke-jellyfin", Version="1"' \
    -d '{"Username":"root","Pw":"synctv-jellyfin"}')"
  jf_token="$(printf '%s' "$jf_auth" | jq -r .AccessToken)"
  curl -fsS -X POST http://127.0.0.1:8097/Library/Refresh -H "X-Emby-Token: $jf_token" >/dev/null
}

ensure_user_room() {
  log "Creating SyncTV test user and room"
  cli_json user create "$SMOKE_USER" --password DevUserPass2026! >"$RESULTS_DIR/user-create.json"
  ROOM_JSON="$(cli_json room create "$SMOKE_ROOM" --username "$SMOKE_USER")"
  printf '%s\n' "$ROOM_JSON" >"$RESULTS_DIR/room-create.json"
  ROOM_ID="$(printf '%s' "$ROOM_JSON" | jq -r '.room.publicId // .room.id // .publicId // .id')"
  [ -n "$ROOM_ID" ] && [ "$ROOM_ID" != "null" ] || die "Room id missing"
}

test_provider_lifecycle() {
  log "Testing provider discovery and remote instance validation"
  cli_json provider available >"$RESULTS_DIR/provider-available.json"
  cli_json provider backends direct-url >"$RESULTS_DIR/provider-backends-direct-url.json"
  cli_json provider backends alist >"$RESULTS_DIR/provider-backends-alist.json"
  cli_json provider list >"$RESULTS_DIR/provider-list.json"
  if cli_json provider create smoke-remote http://127.0.0.1:65535 --provider alist --comment smoke --jwt-secret smoke-provider-secret >"$RESULTS_DIR/provider-create-unreachable.json" 2>"$RESULTS_DIR/provider-create-unreachable.err"; then
    die "Unreachable remote provider instance unexpectedly passed health check"
  fi
  grep -q "health check failed" "$RESULTS_DIR/provider-create-unreachable.err"
}

curl_playback_url() {
  local json_file="$1"
  local url
  mapfile -t curl_args < <(jq -r '
    first(
      (.pullUrls[]? | {url: (.absoluteUrl? // .url?), headers: (.headers // {})}),
      (.. | objects | select(.absoluteUrl? or .url? or .playbackUrl? or .src?) | {url: (.absoluteUrl? // .url? // .playbackUrl? // .src?), headers: (.headers // {})})
    ) as $item
    | ($item.headers | to_entries[]? | "-H\(.key): \(.value)"),
      "URL\($item.url // "")"
  ' "$json_file")
  local header_args=()
  for arg in "${curl_args[@]}"; do
    case "$arg" in
      -H*) header_args+=("-H" "${arg#-H}") ;;
      URL*) url="${arg#URL}" ;;
    esac
  done
  [ -n "$url" ] || die "No playback URL in $json_file"
  case "$url" in
    http://*|https://*) ;;
    /*) url="$BASE_URL$url" ;;
    *) die "Unsupported playback URL in $json_file: $url" ;;
  esac
  curl -fsS -L --max-time 20 --range 0-65535 --max-filesize 1048576 \
    "${header_args[@]}" "$url" -o /tmp/synctv-smoke-playback.bin
  test -s /tmp/synctv-smoke-playback.bin
}

test_direct_url() {
  log "Testing Direct URL provider"
  DIRECT_JSON="$(cli_json media add-url --room-id "$ROOM_ID" --username "$SMOKE_USER" --name direct-mp4 "$STATIC_URL/direct.mp4")"
  printf '%s\n' "$DIRECT_JSON" >"$RESULTS_DIR/direct-media.json"
  DIRECT_MEDIA_ID="$(printf '%s' "$DIRECT_JSON" | jq -r '.media.id // .id')"
  HLS_JSON="$(cli_json media add --room-id "$ROOM_ID" --username "$SMOKE_USER" --source-provider direct-url --name direct-hls --source-config-json "{\"medias\":[{\"url\":\"$STATIC_URL/stream.m3u8\",\"format\":\"hls\"}]}")"
  printf '%s\n' "$HLS_JSON" >"$RESULTS_DIR/direct-hls-media.json"

  cli_json room playback start --room-id "$ROOM_ID" --media-id "$DIRECT_MEDIA_ID" >"$RESULTS_DIR/direct-start.json"
  cli_json room playback get --room-id "$ROOM_ID" >"$RESULTS_DIR/direct-playback.json"
  curl_playback_url "$RESULTS_DIR/direct-playback.json"
  cli_json room playback pause --room-id "$ROOM_ID" >"$RESULTS_DIR/playback-pause.json"
  cli_json room playback seek --room-id "$ROOM_ID" --position 1.5 >"$RESULTS_DIR/playback-seek.json"
  cli_json room playback speed --room-id "$ROOM_ID" --speed 1.25 >"$RESULTS_DIR/playback-speed.json"
  cli_json room playback play --room-id "$ROOM_ID" >"$RESULTS_DIR/playback-play.json"
  cli_json room playback stop --room-id "$ROOM_ID" >"$RESULTS_DIR/playback-stop.json"
}

test_alist() {
  log "Testing OpenList/Alist provider"
  ALIST_LOGIN="$(cli_json provider alist login --username "$SMOKE_USER" --host http://127.0.0.1:5244 --account-username admin --password synctv-openlist)"
  printf '%s\n' "$ALIST_LOGIN" >"$RESULTS_DIR/alist-login.json"
  ALIST_SERVER_ID="$(printf '%s' "$ALIST_LOGIN" | jq -r '.serverId // .bind.serverId // .credential.serverId')"
  cli_json provider alist me --username "$SMOKE_USER" --server-id "$ALIST_SERVER_ID" >"$RESULTS_DIR/alist-me.json"
  cli_json provider alist binds --username "$SMOKE_USER" >"$RESULTS_DIR/alist-binds.json"
  cli_json provider alist list --username "$SMOKE_USER" --server-id "$ALIST_SERVER_ID" --path / --refresh >"$RESULTS_DIR/alist-list.json"
  ALIST_MEDIA="$(cli_json media provider alist --room-id "$ROOM_ID" --username "$SMOKE_USER" --server-id "$ALIST_SERVER_ID" --path /direct.mp4 --name alist-direct)"
  printf '%s\n' "$ALIST_MEDIA" >"$RESULTS_DIR/alist-media.json"
  ALIST_MEDIA_ID="$(printf '%s' "$ALIST_MEDIA" | jq -r '.media.id // .id')"
  ALIST_PLAYLIST="$(cli_json playlist provider alist "Alist Dynamic" --room-id "$ROOM_ID" --username "$SMOKE_USER" --server-id "$ALIST_SERVER_ID" --path /)"
  printf '%s\n' "$ALIST_PLAYLIST" >"$RESULTS_DIR/alist-playlist.json"
  ALIST_PLAYLIST_ID="$(printf '%s' "$ALIST_PLAYLIST" | jq -r '.playlist.id // .id')"
  cli_json media list --room-id "$ROOM_ID" --playlist-id "$ALIST_PLAYLIST_ID" --refresh >"$RESULTS_DIR/alist-dynamic-list.json"
  cli_json room playback start --room-id "$ROOM_ID" --media-id "$ALIST_MEDIA_ID" >"$RESULTS_DIR/alist-start.json"
  cli_json room playback get --room-id "$ROOM_ID" >"$RESULTS_DIR/alist-playback.json"
  curl_playback_url "$RESULTS_DIR/alist-playback.json"
}

find_emby_item() {
  local host="$1"
  local user="$2"
  local password="$3"
  local device="$4"
  local auth token items
  auth="$(curl -fsS -X POST "$host/Users/AuthenticateByName" \
    -H 'Content-Type: application/json' \
    -H "Authorization: MediaBrowser Client=\"SyncTVDev\", Device=\"curl\", DeviceId=\"$device\", Version=\"1\"" \
    -H "X-Emby-Authorization: MediaBrowser Client=\"SyncTVDev\", Device=\"curl\", DeviceId=\"$device\", Version=\"1\"" \
    -d "{\"Username\":\"$user\",\"Pw\":\"$password\"}")"
  token="$(printf '%s' "$auth" | jq -r .AccessToken)"
  for _ in $(seq 1 60); do
    items="$(curl -fsS "$host/Items?Recursive=true&IncludeItemTypes=Video&Fields=Path,MediaSources&Limit=20" -H "X-Emby-Token: $token")"
    if printf '%s' "$items" | jq -e '.Items[]? | select(.Name=="direct" or (.Path | endswith("direct.mp4")))' >/dev/null; then
      printf '%s\n' "$items"
      return
    fi
    sleep 2
  done
  die "Timed out waiting for $host direct.mp4"
}

test_emby_like() {
  local label="$1"
  local host="$2"
  local account="$3"
  local password="$4"
  log "Testing $label provider lifecycle"
  local items login server_id item_id root_id media media_id playlist playlist_id
  items="$(find_emby_item "$host" "$account" "$password" "smoke-$label")"
  printf '%s\n' "$items" >"$RESULTS_DIR/$label-items.json"
  item_id="$(printf '%s' "$items" | jq -r '.Items[] | select(.Name=="direct" or (.Path | endswith("direct.mp4"))) | .Id' | head -n 1)"
  login="$(cli_json provider emby login --username "$SMOKE_USER" --host "$host" --account-username "$account" --password "$password")"
  printf '%s\n' "$login" >"$RESULTS_DIR/$label-login.json"
  server_id="$(printf '%s' "$login" | jq -r '.serverId // .bind.serverId // .credential.serverId')"
  cli_json provider emby me --username "$SMOKE_USER" --server-id "$server_id" >"$RESULTS_DIR/$label-me.json"
  cli_json provider emby binds --username "$SMOKE_USER" >"$RESULTS_DIR/$label-binds.json"
  cli_json provider emby list --username "$SMOKE_USER" --server-id "$server_id" --path "" --limit 20 >"$RESULTS_DIR/$label-list.json"
  root_id="$(jq -r '.items[0].id // empty' "$RESULTS_DIR/$label-list.json")"
  [ -n "$root_id" ] || root_id="$item_id"
  media="$(cli_json media provider emby --room-id "$ROOM_ID" --username "$SMOKE_USER" --server-id "$server_id" --item-id "$item_id" --name "$label-direct")"
  printf '%s\n' "$media" >"$RESULTS_DIR/$label-media.json"
  media_id="$(printf '%s' "$media" | jq -r '.media.id // .id')"
  playlist="$(cli_json playlist provider emby "$label Dynamic" --room-id "$ROOM_ID" --username "$SMOKE_USER" --server-id "$server_id" --item-id "$root_id")"
  printf '%s\n' "$playlist" >"$RESULTS_DIR/$label-playlist.json"
  playlist_id="$(printf '%s' "$playlist" | jq -r '.playlist.id // .id')"
  cli_json media list --room-id "$ROOM_ID" --playlist-id "$playlist_id" --refresh >"$RESULTS_DIR/$label-dynamic-list.json"
  cli_json room playback start --room-id "$ROOM_ID" --media-id "$media_id" >"$RESULTS_DIR/$label-start.json"
  cli_json room playback get --room-id "$ROOM_ID" --stream direct-play >"$RESULTS_DIR/$label-playback.json"
  curl_playback_url "$RESULTS_DIR/$label-playback.json"
  cli_json provider emby logout --username "$SMOKE_USER" --server-id "$server_id" >"$RESULTS_DIR/$label-logout.json"
}

test_bilibili() {
  log "Testing Bilibili anonymous provider paths"
  cli_json provider bilibili parse --username "$SMOKE_USER" 'https://www.bilibili.com/video/BV1xx411c7mD/' >"$RESULTS_DIR/bilibili-parse.json"
  cli_json provider bilibili login-qr --username "$SMOKE_USER" >"$RESULTS_DIR/bilibili-login-qr.json" || true
  local bvid cid media media_id
  bvid="$(jq -r '.candidates[0].media.bilibili.video.bvid // .bvid // .video.bvid // .videos[0].bvid // empty' "$RESULTS_DIR/bilibili-parse.json")"
  cid="$(jq -r '.candidates[0].media.bilibili.video.cid // .cid // .video.cid // .pages[0].cid // .videos[0].cid // empty' "$RESULTS_DIR/bilibili-parse.json")"
  [ -n "$bvid" ] && [ -n "$cid" ] || die "Bilibili parse did not return bvid and cid"
  media="$(cli_json media provider bilibili video --room-id "$ROOM_ID" --username "$SMOKE_USER" --bvid "$bvid" --cid "$cid" --name bilibili-smoke)"
  printf '%s\n' "$media" >"$RESULTS_DIR/bilibili-media.json"
  media_id="$(printf '%s' "$media" | jq -r '.media.id // .id')"
  cli_json room playback start --room-id "$ROOM_ID" --media-id "$media_id" >"$RESULTS_DIR/bilibili-start.json"
  cli_json room playback get --room-id "$ROOM_ID" >"$RESULTS_DIR/bilibili-playback.json"
  curl_playback_url "$RESULTS_DIR/bilibili-playback.json"
}

test_live_proxy() {
  log "Testing LiveProxy provider"
  local media media_id
  media="$(cli_json media add --room-id "$ROOM_ID" --username "$SMOKE_USER" --source-provider live-proxy --name live-proxy-flv --source-config-json "{\"url\":\"$STATIC_URL/live.flv\"}")"
  printf '%s\n' "$media" >"$RESULTS_DIR/live-proxy-media.json"
  media_id="$(printf '%s' "$media" | jq -r '.media.id // .id')"
  cli_json room playback start --room-id "$ROOM_ID" --media-id "$media_id" >"$RESULTS_DIR/live-proxy-start.json"
  cli_json room playback get --room-id "$ROOM_ID" >"$RESULTS_DIR/live-proxy-playback.json"
  curl_playback_url "$RESULTS_DIR/live-proxy-playback.json" || true
}

test_crud() {
  log "Testing media and playlist CRUD"
  local playlist playlist_id second second_id
  playlist="$(cli_json playlist create "Static Smoke" --room-id "$ROOM_ID" --username "$SMOKE_USER")"
  printf '%s\n' "$playlist" >"$RESULTS_DIR/static-playlist.json"
  playlist_id="$(printf '%s' "$playlist" | jq -r '.playlist.id // .id')"
  cli_json playlist update --room-id "$ROOM_ID" --name "Static Smoke Renamed" "$playlist_id" >"$RESULTS_DIR/static-playlist-update.json"
  second="$(cli_json media add-url --room-id "$ROOM_ID" --username "$SMOKE_USER" --playlist-id "$playlist_id" --name crud-direct "$STATIC_URL/direct.mp4")"
  printf '%s\n' "$second" >"$RESULTS_DIR/crud-media.json"
  second_id="$(printf '%s' "$second" | jq -r '.media.id // .id')"
  cli_json media update --room-id "$ROOM_ID" --name crud-direct-renamed "$second_id" >"$RESULTS_DIR/crud-media-update.json"
  cli_json media list --room-id "$ROOM_ID" --playlist-id "$playlist_id" >"$RESULTS_DIR/crud-media-list.json"
  cli_json media delete --room-id "$ROOM_ID" --force "$second_id" >"$RESULTS_DIR/crud-media-delete.json"
  cli_json playlist delete --room-id "$ROOM_ID" --force "$playlist_id" >"$RESULTS_DIR/static-playlist-delete.json"
}

main() {
  command -v jq >/dev/null || die "jq is required"
  command -v ffmpeg >/dev/null || die "ffmpeg is required"
  ensure_bin
  start_stack
  make_media
  prepare_media_servers
  start_synctv
  ensure_user_room
  test_provider_lifecycle
  test_direct_url
  test_alist
  test_emby_like emby http://127.0.0.1:8096 MyEmbyUser synctv-emby
  test_emby_like jellyfin http://127.0.0.1:8097 root synctv-jellyfin
  test_bilibili
  test_live_proxy
  test_crud
  log "Smoke results written to $RESULTS_DIR"
}

main "$@"
