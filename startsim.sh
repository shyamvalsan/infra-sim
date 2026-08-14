#!/usr/bin/env bash
#
# startsim - bring up the Infra-Sim console on a machine that has Docker.
#
# Two ways in, same script:
#
#   ./startsim.sh                                     # inside a clone
#   curl -fsSL <raw-url>/startsim.sh | sudo bash      # on a bare machine
#
# `bash`, not `sh`: piping ignores the shebang, and this script uses BASH_SOURCE
# to tell "run from a clone" apart from "piped in, so clone first".
#
# It checks what it needs and installs nothing. Installing Docker for you would
# mean guessing your distro and your intentions; a missing dependency stops the
# script with the command to fix it instead.
#
# No Rust toolchain is required: the binaries are built inside Docker, which is
# already mandatory because a simulation is a container.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
GRAY='\033[0;90m'
NC='\033[0m'

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

info() { echo -e >&2 "${GREEN}==>${NC} $*"; }
warn() { echo -e >&2 "${YELLOW}[warn]${NC} $*"; }
die() { echo -e >&2 "${RED}[error]${NC} $*"; exit 1; }

# Kept before the argument loop consumes them, so the "try: sudo ..." hint can
# repeat what the operator actually typed.
ORIGINAL_ARGS="$*"

REPO_URL="${INFRA_SIM_REPO_URL:-https://github.com/shyamvalsan/infra-sim}"
CLONE_DIR="${INFRA_SIM_SRC:-/opt/infra-sim-src}"
BUILDER_IMAGE="infra-sim-builder:local"
BIND="127.0.0.1:8080"
REPO=""
REBUILD=no
BUILD=yes
ENV_ARGS=()

usage() {
  cat >&2 <<'EOF'
usage: startsim.sh [options]

  --repo PATH      use this checkout instead of the working directory
  --environment P  environment.yaml the console should drive (default: the host
                   install; pass a simulation's own to get its scenarios)
  --bind HOST:PORT console listen address (default 127.0.0.1:8080)
  --rebuild        rebuild the binaries even if they are already present
  --no-build       skip the build; fail if binaries are missing
  --help           this text

environment:
  INFRA_SIM_SRC        where to clone when not run from a checkout
                       (default /opt/infra-sim-src)
  INFRA_SIM_REPO_URL   clone source
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="${2:?--repo needs a path}"; shift 2 ;;
    --environment) ENV_ARGS=(--environment "${2:?--environment needs a path}"); shift 2 ;;
    --bind) BIND="${2:?--bind needs host:port}"; shift 2 ;;
    --rebuild) REBUILD=yes; shift ;;
    --no-build) BUILD=no; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage; die "unknown option '$1'" ;;
  esac
done

# --- preflight ---------------------------------------------------------------
#
# Every check names the fix. `sim-docker.sh` established the pattern; the
# difference here is that this runs before anything else on a machine the
# operator may never have used for this.

install_hint() {
  # Best effort, and honest about it: an unknown distro gets pointed at the
  # upstream instructions rather than a guessed package manager.
  local pkg="$1"
  if command -v apt-get >/dev/null 2>&1; then echo "sudo apt-get install -y $pkg"
  elif command -v dnf >/dev/null 2>&1; then echo "sudo dnf install -y $pkg"
  elif command -v yum >/dev/null 2>&1; then echo "sudo yum install -y $pkg"
  elif command -v pacman >/dev/null 2>&1; then echo "sudo pacman -S --noconfirm $pkg"
  elif command -v zypper >/dev/null 2>&1; then echo "sudo zypper install -y $pkg"
  elif command -v brew >/dev/null 2>&1; then echo "brew install $pkg"
  else echo "install '$pkg' with your platform's package manager"
  fi
}

preflight() {
  local failed=no

  # Root: the console writes under /etc/netdata and drives docker. Checked first
  # because it is the one thing the operator cannot fix from inside the script.
  if [ "$(id -u)" -ne 0 ]; then
    echo -e >&2 "${RED}[error]${NC} startsim must run as root - the console writes under /etc/netdata and drives docker."
    echo -e >&2 "        try: ${YELLOW}sudo $0 ${ORIGINAL_ARGS}${NC}"
    failed=yes
  fi

  if ! command -v docker >/dev/null 2>&1; then
    echo -e >&2 "${RED}[error]${NC} docker is not installed. It is required: every simulation runs as a container, and the binaries are built in one."
    echo -e >&2 "        install it: ${YELLOW}https://docs.docker.com/engine/install/${NC}"
    failed=yes
  elif ! docker info >/dev/null 2>&1; then
    echo -e >&2 "${RED}[error]${NC} docker is installed but its daemon is not answering."
    echo -e >&2 "        try: ${YELLOW}sudo systemctl start docker${NC}   (or add yourself to the 'docker' group)"
    failed=yes
  fi

  # python3: scripts/sim-docker.sh uses it to rewrite the scenario control file
  # and to read the agent's node list. Undeclared until now.
  if ! command -v python3 >/dev/null 2>&1; then
    echo -e >&2 "${RED}[error]${NC} python3 is not installed. scripts/sim-docker.sh needs it to drive scenarios."
    echo -e >&2 "        install it: ${YELLOW}$(install_hint python3)${NC}"
    failed=yes
  fi

  [ "$failed" = no ] || die "preflight failed - nothing was installed or changed."
  info "preflight passed: docker, python3, root"
}

# --- locate or fetch the checkout --------------------------------------------

is_checkout() {
  [ -f "$1/Cargo.toml" ] && [ -d "$1/crates/sim-console" ] && [ -d "$1/specs" ]
}

locate_repo() {
  if [ -n "$REPO" ]; then
    is_checkout "$REPO" || die "'$REPO' is not an infra-sim checkout"
    REPO="$(cd "$REPO" && pwd)"
    return
  fi

  # The script's own directory, so `./startsim.sh` works from anywhere in a
  # clone. BASH_SOURCE is empty when piped from curl, which is the signal to
  # clone instead.
  local here=""
  [ -n "${BASH_SOURCE[0]:-}" ] && here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

  if [ -n "$here" ] && is_checkout "$here"; then
    REPO="$here"
    info "using this checkout: $REPO"
    return
  fi
  if is_checkout "$PWD"; then
    REPO="$PWD"
    info "using this checkout: $REPO"
    return
  fi

  # Piped-from-curl path: fetch the source we are going to build.
  command -v git >/dev/null 2>&1 \
    || die "git is not installed, and startsim needs it to fetch the source. install it: $(install_hint git)"

  if is_checkout "$CLONE_DIR"; then
    info "updating existing source at $CLONE_DIR"
    run git -C "$CLONE_DIR" pull --ff-only || warn "could not fast-forward; building what is on disk"
  else
    info "cloning $REPO_URL into $CLONE_DIR"
    run mkdir -p "$(dirname "$CLONE_DIR")"
    run git clone --depth 1 "$REPO_URL" "$CLONE_DIR"
  fi
  is_checkout "$CLONE_DIR" || die "'$CLONE_DIR' does not look like an infra-sim checkout after cloning"
  REPO="$CLONE_DIR"
}

# --- build -------------------------------------------------------------------

build_binaries() {
  local plugin="$REPO/target/release/infra-sim"
  local console="$REPO/target/release/infra-sim-console"

  if [ "$BUILD" = no ]; then
    [ -x "$plugin" ] && [ -x "$console" ] || die "--no-build was given but the binaries are missing under $REPO/target/release"
    info "skipping build, using the binaries already present"
    return
  fi

  if [ "$REBUILD" = no ] && [ -x "$plugin" ] && [ -x "$console" ]; then
    info "binaries already present - use --rebuild to build again"
    return
  fi

  info "building infra-sim and infra-sim-console in Docker (no Rust toolchain needed)"
  info "the first build compiles the dependency tree and takes a couple of minutes"
  run docker build -f "$REPO/docker/builder.Dockerfile" -t "$BUILDER_IMAGE" "$REPO"

  # Copy out of a throwaway container rather than bind-mounting the checkout,
  # which would leave root-owned build artifacts in an operator's repo.
  local cid
  cid="$(docker create "$BUILDER_IMAGE")" || die "could not create a container from $BUILDER_IMAGE"
  # shellcheck disable=SC2064
  trap "docker rm -f '$cid' >/dev/null 2>&1 || true" EXIT

  run mkdir -p "$REPO/target/release"
  run docker cp "$cid:/out/infra-sim" "$plugin"
  run docker cp "$cid:/out/infra-sim-console" "$console"
  run docker rm -f "$cid" >/dev/null
  trap - EXIT

  run chmod 0755 "$plugin" "$console"

  # Give the artifacts the checkout's owner. Running as root would otherwise
  # leave a developer unable to rebuild with cargo afterwards - the same reason
  # provision.rs inherits ownership for files it generates.
  local owner
  owner="$(stat -c '%u:%g' "$REPO" 2>/dev/null || echo "")"
  if [ -n "$owner" ] && [ "$owner" != "0:0" ]; then
    run chown "$owner" "$plugin" "$console"
    run chown "$owner" "$REPO/target" "$REPO/target/release" 2>/dev/null || true
  fi

  info "built: $(basename "$plugin"), $(basename "$console")"
}

# --- go ----------------------------------------------------------------------

preflight
locate_repo
build_binaries

# --- point the console at the right simulation --------------------------------
#
# The console derives its scenario list from `--environment`'s parent directory,
# and that argument defaults to the host install under /etc/netdata. A
# containerised simulation keeps its environment, control file and scenarios
# under /var/lib/infra-sim/<name>/ instead, so with the default the Run tab comes
# up with **no scenarios at all** - measured: 0 against the default path, 6
# against a simulation's own directory. The console adopts a running container for
# metrics and for the control file it writes, but not for this, which is
# `SOW-0013`'s territory to fix properly.
#
# startsim's job stops at starting the UI. Creating a fleet, claiming it and
# tearing it down are the operator's decisions, made in the console - so this
# reports what it found and does not choose. Pass --environment yourself (or
# `startsim --environment ...`, which is forwarded) to drive a specific one.
STATE_DIR="${INFRA_SIM_STATE:-/var/lib/infra-sim}"

report_simulations() {
  [ -d "$STATE_DIR" ] || return 0

  local found=()
  local e
  for e in "$STATE_DIR"/*/environment.yaml; do
    [ -f "$e" ] && found+=("$e")
  done
  [ "${#found[@]}" -gt 0 ] || return 0

  info "${#found[@]} existing simulation(s) found. The console shows a running one's"
  info "metrics automatically, but its Run tab reads scenarios from --environment,"
  info "which defaults to the host install. To drive one of these instead, restart with:"
  for e in "${found[@]}"; do
    echo -e >&2 "${GRAY}      --environment $e${NC}"
  done
}

report_simulations

info "starting the console on http://${BIND}"
echo -e >&2 "${GRAY}    Open it, describe a fleet, and create it. Ctrl-C here stops the console;${NC}"
echo -e >&2 "${GRAY}    simulations keep running in their own containers until you tear them down.${NC}"

cd "$REPO"
exec "$REPO/target/release/infra-sim-console" --repo "$REPO" --bind "$BIND" "${ENV_ARGS[@]+"${ENV_ARGS[@]}"}"
