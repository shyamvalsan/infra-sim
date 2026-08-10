#!/usr/bin/env bash
#
# Run a simulation in its own container, with its own agent.
#
# The host agent is never touched. Each simulation gets its own identity, so it
# can be claimed into its own Space and torn down completely - neither of which
# is possible when a simulation installs into the operator's own agent.
#
# Usage:
#   sim-docker.sh build [--netdata-tag stable]
#   sim-docker.sh create <name> <environment.yaml> [--port N] [--claim] [--rooms IDS]
#   sim-docker.sh list
#   sim-docker.sh status <name>
#   sim-docker.sh scenario <name> trigger|resolve <scenario>
#   sim-docker.sh logs <name> start|stop
#   sim-docker.sh shell <name>
#   sim-docker.sh teardown <name>
#
# The claim token is read from $NETDATA_CLAIM_TOKEN or prompted for. It is never
# written to the repo and never echoed.

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; GRAY='\033[0;90m'; NC='\033[0m'

run() {
  printf >&2 "${GRAY}$(pwd) >${NC} "
  printf >&2 "${YELLOW}"
  printf >&2 "%q " "$@"
  printf >&2 "${NC}\n"
  if ! "$@"; then
    local exit_code=$?
    echo -e >&2 "${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e >&2 "${RED}[ERROR]${NC} Command failed with exit code ${exit_code}: ${YELLOW}$1${NC}"
    echo -e >&2 "${RED}        Full command:${NC} $*"
    echo -e >&2 "${RED}        Working dir:${NC} $(pwd)"
    echo -e >&2 "${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    return $exit_code
  fi
}

die() { echo -e >&2 "${RED}[ERROR]${NC} $*"; exit 1; }
info() { echo -e >&2 "${GREEN}==>${NC} $*"; }
# A degraded simulation is still a simulation: telemetry that fails to start must
# warn, never abort a create that otherwise worked.
warn() { echo -e >&2 "${YELLOW}[WARN]${NC} $*"; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="infra-sim:latest"
# Where each simulation's payload lives on the host. Not in the repo: these are
# per-demo instances and may carry a prospect's name.
#
# A fixed path rather than $HOME, because the console runs under sudo and this
# script may not - keying off HOME had the two disagreeing about where a
# simulation lived.
STATE_DIR="${INFRA_SIM_STATE_DIR:-/var/lib/infra-sim}"
LABEL="infra-sim.simulation"

container_of() { echo "infra-sim-$1"; }

require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed"
  docker info >/dev/null 2>&1 || die "cannot talk to docker (is the daemon running, are you in the docker group?)"
}

require_image() {
  docker image inspect "$IMAGE" >/dev/null 2>&1 \
    || die "image $IMAGE is missing. Build it first:  $0 build"
}

# --- build -----------------------------------------------------------------
cmd_build() {
  # netdata's nightly channel. See docker/Dockerfile for why this is not stable.
  local tag=latest
  while [ $# -gt 0 ]; do
    case "$1" in
      --netdata-tag) tag="$2"; shift 2 ;;
      *) die "unknown option '$1'" ;;
    esac
  done
  require_docker
  [ -x "$REPO/target/release/infra-sim" ] \
    || die "target/release/infra-sim is missing. Build it first:  cargo build --release"

  # Docker can only COPY from the build context, and the context is kept to
  # exactly the binary so an unrelated repo file cannot end up in the image.
  local ctx; ctx="$(mktemp -d)"
  trap 'rm -rf "$ctx"' RETURN
  run cp "$REPO/target/release/infra-sim" "$ctx/infra-sim"
  run cp "$REPO/docker/Dockerfile" "$ctx/Dockerfile"
  run docker build --build-arg "NETDATA_TAG=$tag" -t "$IMAGE" "$ctx"
  info "built $IMAGE on netdata/netdata:$tag"
}

# --- create ----------------------------------------------------------------
cmd_create() {
  local name="${1:-}" env_file="${2:-}"; shift 2 || true
  [ -n "$name" ] && [ -n "$env_file" ] || die "usage: $0 create <name> <environment.yaml> [--port N] [--claim] [--rooms IDS]"
  [ -f "$env_file" ] || die "no such environment file: $env_file"

  local port="" claim=no rooms="" url="https://app.netdata.cloud"
  while [ $# -gt 0 ]; do
    case "$1" in
      --port) port="$2"; shift 2 ;;
      --claim) claim=yes; shift ;;
      --rooms) rooms="$2"; shift 2 ;;
      --url) url="$2"; shift 2 ;;
      *) die "unknown option '$1'" ;;
    esac
  done

  require_docker; require_image
  local container; container="$(container_of "$name")"
  docker ps -a --format '{{.Names}}' | grep -qx "$container" \
    && die "simulation '$name' already exists. Tear it down first:  $0 teardown $name"

  [ -n "$port" ] || port="$(free_port)"

  # Assemble the payload the container mounts: the environment with its paths
  # rewritten to the container's layout, beside the specs and scenarios it
  # names. Same layout the console installs on a host.
  #
  # Mounted as a directory, not file by file. A bind-mounted *file* is bound to
  # one inode, and anything that writes by replace-and-rename - sed -i, most
  # editors - leaves the container reading the old inode forever. Triggering a
  # scenario did exactly that: the host saw it, the container did not.
  local dir="$STATE_DIR/$name"
  run mkdir -p "$dir/specs/generated" "$dir/scenarios" "$dir/journal"
  run cp "$env_file" "$dir/environment.yaml"
  sed -i 's|generator: \.\./specs/|generator: specs/|; s|specs: \.\./specs|specs: specs|; s|scenarios: \.\./scenarios|scenarios: scenarios|' "$dir/environment.yaml"
  run cp -r "$REPO/specs/." "$dir/specs/"
  run cp -r "$REPO/scenarios/." "$dir/scenarios/"
  # The container's own agent is labelled with the fleet's own coordinates, so
  # the one node beside the simulated ones is not the only unplaced node in the
  # Space. Read from the environment's `site:` block; absent, the two label lines
  # are dropped rather than defaulting to 0,0 in the Gulf of Guinea.
  local lat lon
  lat="$(awk '/^site:/{f=1;next} f&&/^  latitude:/{print $2;exit} f&&/^[^ ]/{exit}' "$dir/environment.yaml")"
  lon="$(awk '/^site:/{f=1;next} f&&/^  longitude:/{print $2;exit} f&&/^[^ ]/{exit}' "$dir/environment.yaml")"
  sed "s|__SIM_NAME__|$name|g" "$REPO/docker/netdata.conf.template" > "$dir/netdata.conf"
  if [ -n "$lat" ] && [ -n "$lon" ]; then
    sed -i "s|__SIM_LATITUDE__|$lat|; s|__SIM_LONGITUDE__|$lon|" "$dir/netdata.conf"
  else
    sed -i '/__SIM_LATITUDE__/d; /__SIM_LONGITUDE__/d' "$dir/netdata.conf"
  fi
  # An empty control file so a scenario can be triggered without creating it.
  [ -f "$dir/control.yaml" ] || echo "active: []" > "$dir/control.yaml"

  local -a claim_env=()
  if [ "$claim" = yes ]; then
    local token="${NETDATA_CLAIM_TOKEN:-}"
    if [ -z "$token" ]; then
      read -r -s -p "Netdata Cloud claim token: " token; echo >&2
    fi
    [ -n "$token" ] || die "a claim token is required with --claim"
    # Visible in `docker inspect` for the life of the container. This is the
    # mechanism netdata documents for containers; the alternatives (baking it
    # into an image, or a file on a shared volume) are worse.
    claim_env+=(-e "NETDATA_CLAIM_TOKEN=$token" -e "NETDATA_CLAIM_URL=$url")
    [ -n "$rooms" ] && claim_env+=(-e "NETDATA_CLAIM_ROOMS=$rooms")
    info "claiming into $url${rooms:+ (room $rooms)}"
  fi

  run docker run -d \
    --name "$container" \
    --label "$LABEL=$name" \
    --restart unless-stopped \
    -p "127.0.0.1:$port:19999" \
    -v "$dir/netdata.conf:/etc/netdata/netdata.conf:ro" \
    -v "$dir:/etc/netdata/infra-sim" \
    -v "$dir/journal:/var/log/journal/remote" \
    "${claim_env[@]}" \
    "$IMAGE" >/dev/null

  info "simulation '$name' is starting"

  # Telemetry is part of being a simulation, not a second command. Every
  # simulation built before this shipped with empty logs, because `logs start`
  # was a step an operator had to remember and nobody did.
  #
  # Both writers wait for the plugin file to exist inside the container - the
  # agent copies nothing, we mount it, but the container needs a moment to come
  # up before `docker exec` will work.
  cmd_telemetry "$name" start || warn "telemetry did not start; $0 telemetry $name start to retry"

  echo "  dashboard : http://127.0.0.1:$port"
  echo "  payload   : $dir"
  echo "  nodes appear within about a minute (the agent scans for plugins every 60s)"
}

# Correlated logs and OpenTelemetry, as one unit: both are per-simulation side
# processes that must die with it, and an operator has no reason to want one
# without the other.
cmd_telemetry() {
  local name="${1:?usage: $0 telemetry <name> start|stop|status}"
  local action="${2:-start}"
  require_docker
  local container; container="$(container_of "$name")"
  local plugin=/etc/netdata/custom-plugins.d/infra-sim.plugin
  local env=/etc/netdata/infra-sim/environment.yaml

  case "$action" in
    start)
      # Wait for the container to accept exec at all.
      local i
      for i in $(seq 1 30); do
        docker exec "$container" sh -c "test -f $plugin" 2>/dev/null && break
        sleep 1
      done
      docker exec "$container" sh -c "test -f $plugin" 2>/dev/null \
        || { warn "the plugin is not visible inside '$name' yet"; return 1; }

      # Correlated logs: each node becomes its own log source. Needs
      # systemd-journal-remote, which the image installs.
      docker exec -d "$container" sh -c \
        "$plugin --logs --environment $env >/tmp/infra-sim-logs.log 2>&1"

      # OpenTelemetry: the application tier's logs and traces, to the agent's own
      # OTLP receiver on loopback inside the container.
      docker exec -d "$container" sh -c \
        "$plugin --otlp --environment $env >/tmp/infra-sim-otlp.log 2>&1"

      sleep 3
      cmd_telemetry "$name" status
      ;;
    stop)
      # Matched on the plugin path plus its mode, never on a bare name: this
      # machine may be running other simulations.
      docker exec "$container" sh -c "pkill -f '$plugin --logs' || true"
      docker exec "$container" sh -c "pkill -f '$plugin --otlp' || true"
      info "telemetry stopped in '$name'"
      ;;
    status)
      local journals otlp
      journals="$(docker exec "$container" sh -c 'ls /var/log/journal/remote/ 2>/dev/null | wc -l' 2>/dev/null | tr -d "[:space:]")"
      echo "  logs      : ${journals:-0} journal file(s) in the container"
      # The last line, not the first: the first is always the start-up race
      # against an agent that is still opening its OTLP port.
      otlp="$(docker exec "$container" sh -c 'tail -1 /tmp/infra-sim-otlp.log 2>/dev/null' 2>/dev/null)"
      if [ -n "$otlp" ]; then
        echo "  otel      : ${otlp#infra-sim otlp: }"
      else
        echo "  otel      : not started"
      fi
      # What happens to traces depends on the agent build, so this reports rather
      # than promises. Netdata's stable image has no `traces:` section at all;
      # newer builds store them, and no build can display them yet.
      echo "              (no agent build can display traces yet, whatever it accepts)"
      ;;
    *) die "action must be start, stop or status" ;;
  esac
}

free_port() {
  local p
  for p in $(seq 19990 -1 19900); do
    ss -tln 2>/dev/null | grep -q ":$p " || { echo "$p"; return; }
  done
  die "no free port in 19900-19990"
}

# --- lifecycle -------------------------------------------------------------
cmd_list() {
  require_docker
  printf "%-20s %-12s %-26s %s\n" SIMULATION STATE DASHBOARD PAYLOAD
  docker ps -a --filter "label=$LABEL" --format '{{.Label "infra-sim.simulation"}}\t{{.State}}\t{{.Ports}}' \
  | while IFS=$'\t' read -r name state ports; do
      local_port="$(echo "$ports" | grep -oE '127\.0\.0\.1:[0-9]+' | head -1 | cut -d: -f2)"
      printf "%-20s %-12s %-26s %s\n" "$name" "$state" "http://127.0.0.1:${local_port:-?}" "$STATE_DIR/$name"
    done
}

cmd_status() {
  local name="${1:?usage: $0 status <name>}"
  require_docker
  local container; container="$(container_of "$name")"
  local port; port="$(docker port "$container" 19999/tcp 2>/dev/null | head -1 | cut -d: -f2)"
  [ -n "$port" ] || die "simulation '$name' is not running"

  echo "container : $(docker inspect -f '{{.State.Status}} (started {{.State.StartedAt}})' "$container")"
  echo "dashboard : http://127.0.0.1:$port"
  curl -s -m 5 "http://127.0.0.1:$port/api/v3/nodes" 2>/dev/null | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
except Exception:
    print('nodes     : agent not answering yet'); raise SystemExit
nodes = d.get('nodes', [])
print('nodes     : %d' % len(nodes))
for n in nodes:
    print('            %s  (%s)' % (n['nm'], n.get('state', '?')))
" || echo "nodes     : agent not answering yet"
  echo "claim     : $(docker exec "$container" sh -c 'cat /var/lib/netdata/cloud.d/claimed_id 2>/dev/null || echo "not claimed"')"
}

cmd_scenario() {
  local name="${1:?usage: $0 scenario <name> trigger|resolve <scenario>}"
  local action="${2:?trigger or resolve}"
  local scenario="${3:?scenario name}"
  local dir="$STATE_DIR/$name"
  [ -f "$dir/control.yaml" ] || die "simulation '$name' has no control file"

  # Rewritten in place, truncating rather than replacing: the container has this
  # directory bind-mounted, and a replace-and-rename would leave it reading a
  # stale inode.
  python3 "$REPO/scripts/control_file.py" "$dir/control.yaml" "$action" "$scenario" \
    || die "could not update the control file"

  case "$action" in
    trigger) info "triggered '$scenario'" ;;
    resolve) info "resolving '$scenario' - it unwinds over the next three minutes" ;;
  esac
}

# Kept as an alias: `logs` is what this was called before telemetry grew a
# second signal, and it is in muscle memory and in the docs.
cmd_logs() {
  cmd_telemetry "$@"
}

cmd_shell() {
  local name="${1:?usage: $0 shell <name>}"
  require_docker
  run docker exec -it "$(container_of "$name")" bash
}

cmd_teardown() {
  local name="${1:?usage: $0 teardown <name>}"
  require_docker
  local container; container="$(container_of "$name")"
  docker ps -a --format '{{.Names}}' | grep -qx "$container" || die "no such simulation: $name"

  # The container carries the agent, its database and every vnode's history, so
  # removing it removes the simulation completely. Nothing to disarm, no stale
  # nodes, no config left in anyone's /etc/netdata.
  run docker rm -f "$container"

  local dir="$STATE_DIR/$name"
  if [ -d "$dir" ]; then
    local archive="$REPO/archive/$name-$(date +%s)"
    run mkdir -p "$archive"
    run cp "$dir/environment.yaml" "$archive/environment.yaml"
    run cp -r "$dir/scenarios" "$archive/scenarios"
    run rm -rf "$dir"
    info "archived to $archive"
  fi
  info "simulation '$name' is gone. The Cloud Space, if it was claimed, is yours to delete."
}

# --- dispatch --------------------------------------------------------------
case "${1:-}" in
  build)    shift; cmd_build "$@" ;;
  create)   shift; cmd_create "$@" ;;
  list)     shift; cmd_list "$@" ;;
  status)   shift; cmd_status "$@" ;;
  scenario) shift; cmd_scenario "$@" ;;
  logs)     shift; cmd_logs "$@" ;;
  telemetry) shift; cmd_telemetry "$@" ;;
  shell)    shift; cmd_shell "$@" ;;
  teardown) shift; cmd_teardown "$@" ;;
  *)
    sed -n '3,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
