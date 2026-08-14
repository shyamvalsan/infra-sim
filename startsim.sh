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
# How to invoke docker. Replaced by preflight when only the invoking user's
# rootless daemon answers.
DOCKER="docker"
BIND="127.0.0.1:19995"
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
  --bind HOST:PORT console listen address (default 127.0.0.1:19995)
  --rebuild        rebuild the binaries even if they are already present
  --no-build       skip the build; fail if binaries are missing
  --help           this text

environment:
  INFRA_SIM_STATE_DIR  where simulations keep their state
                       (default /var/lib/infra-sim)
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

  # Platform decides *how* the console runs, not whether.
  #
  # The binaries are built in an Alpine container and are therefore Linux ELF. On
  # Linux they exec on the host. On macOS the host cannot exec them - which cost a
  # real attempt several minutes of build before dying at the final exec - so the
  # console runs in a container instead and drives the host's Docker through the
  # mounted socket. Same binary, different delivery.
  KERNEL="$(uname -s 2>/dev/null || echo unknown)"
  case "$KERNEL" in
    Linux) CONSOLE_MODE=host ;;
    Darwin)
      CONSOLE_MODE=container
      MACOS_EXPERIMENTAL=yes
      ;;
    *)
      echo -e >&2 "${RED}[error]${NC} startsim supports Linux and macOS; this is '${KERNEL}'."
      echo -e >&2 "        The console needs either a Linux host or a Docker that runs Linux"
      echo -e >&2 "        containers. Use a Linux machine or VM."
      die "unsupported host platform - nothing was installed or changed."
      ;;
  esac

  # Root: the console writes under /etc/netdata and drives docker. Checked first
  # because it is the one thing the operator cannot fix from inside the script.
  # Root, on Linux only. In container mode the console writes nothing on the host
  # except the state directory, and Docker Desktop is reached as the ordinary user -
  # asking for sudo there would break the socket it needs.
  if [ "$CONSOLE_MODE" = host ] && [ "$(id -u)" -ne 0 ]; then
    echo -e >&2 "${RED}[error]${NC} startsim must run as root - the console writes under /etc/netdata and drives docker."
    echo -e >&2 "        try: ${YELLOW}sudo $0 ${ORIGINAL_ARGS}${NC}"
    failed=yes
  fi

  if ! command -v docker >/dev/null 2>&1; then
    echo -e >&2 "${RED}[error]${NC} docker is not installed. It is required: every simulation runs as a container, and the binaries are built in one."
    echo -e >&2 "        install it: ${YELLOW}https://docs.docker.com/engine/install/${NC}"
    failed=yes
  elif ! docker info >/dev/null 2>&1; then
    # Rootless Docker runs a per-user daemon on a socket under $XDG_RUNTIME_DIR,
    # which root cannot see. Since this script requires root for the console, a
    # healthy rootless install failed here with "daemon is not answering" - the
    # message was accurate and the diagnosis was wrong. Retry as the user who
    # invoked sudo before concluding anything.
    if [ -n "${SUDO_USER:-}" ] && sudo -u "$SUDO_USER" docker info >/dev/null 2>&1; then
      DOCKER="sudo -u $SUDO_USER docker"
      info "docker answers as '$SUDO_USER' but not as root - rootless install, using theirs"
    else
      echo -e >&2 "${RED}[error]${NC} docker is installed but its daemon is not answering."
      echo -e >&2 "        if it is not running:  ${YELLOW}sudo systemctl start docker${NC}"
      echo -e >&2 "        if it is rootless:     ${YELLOW}run without sudo, or start the user daemon:${NC}"
      echo -e >&2 "                               ${YELLOW}systemctl --user start docker${NC}"
      echo -e >&2 "        checked as: root${SUDO_USER:+ and as $SUDO_USER}"
      failed=yes
    fi
  fi

  # python3: scripts/sim-docker.sh uses it to rewrite the scenario control file
  # and to read the agent's node list. Only needed where that script runs - in
  # container mode it runs inside the console image, which installs it.
  if [ "$CONSOLE_MODE" = host ] && ! command -v python3 >/dev/null 2>&1; then
    echo -e >&2 "${RED}[error]${NC} python3 is not installed. scripts/sim-docker.sh needs it to drive scenarios."
    echo -e >&2 "        install it: ${YELLOW}$(install_hint python3)${NC}"
    failed=yes
  fi

  [ "$failed" = no ] || die "preflight failed - nothing was installed or changed."
  if [ "$CONSOLE_MODE" = host ]; then
    info "preflight passed: docker, python3, root"
  else
    info "preflight passed: docker (console will run in a container)"
  fi
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
  run $DOCKER build -f "$REPO/docker/builder.Dockerfile" -t "$BUILDER_IMAGE" "$REPO"

  # Copy out of a throwaway container rather than bind-mounting the checkout,
  # which would leave root-owned build artifacts in an operator's repo.
  local cid
  cid="$($DOCKER create "$BUILDER_IMAGE")" || die "could not create a container from $BUILDER_IMAGE"
  # shellcheck disable=SC2064
  trap "$DOCKER rm -f '$cid' >/dev/null 2>&1 || true" EXIT

  run mkdir -p "$REPO/target/release"
  run $DOCKER cp "$cid:/out/infra-sim" "$plugin"
  run $DOCKER cp "$cid:/out/infra-sim-console" "$console"
  run $DOCKER rm -f "$cid" >/dev/null
  trap - EXIT

  run chmod 0755 "$plugin" "$console"

  # Give the artifacts the checkout's owner. Running as root would otherwise
  # leave a developer unable to rebuild with cargo afterwards - the same reason
  # provision.rs inherits ownership for files it generates.
  local owner
  owner="$(stat -c '%u:%g' "$REPO" 2>/dev/null || stat -f '%u:%g' "$REPO" 2>/dev/null || echo "")"
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
# /var/lib is not shared with Docker Desktop by default, and is not where a Mac
# keeps user state. $HOME is shared, so simulations live there instead.
if [ "${KERNEL:-Linux}" = Darwin ]; then
  STATE_DIR="${INFRA_SIM_STATE_DIR:-$HOME/.infra-sim}"
else
  STATE_DIR="${INFRA_SIM_STATE_DIR:-/var/lib/infra-sim}"
fi
export INFRA_SIM_STATE_DIR="$STATE_DIR"

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

if [ "$CONSOLE_MODE" = host ]; then
  exec "$REPO/target/release/infra-sim-console" --repo "$REPO" --bind "$BIND" "${ENV_ARGS[@]+"${ENV_ARGS[@]}"}"
fi

# Container mode. The repo and the state directory are mounted at identical paths,
# because sim-docker.sh hands `-v <path>:...` to the daemon and the daemon resolves
# it against the host - mount them elsewhere and every simulation would bind an
# empty directory.
if [ "${MACOS_EXPERIMENTAL:-no}" = yes ]; then
  warn "macOS support is EXPERIMENTAL and not yet verified on a Mac."
  warn "What works: the UI runs, in a container, driving your Docker through its socket."
  warn "Known gap: the console reaches an adopted simulation at 127.0.0.1, which from"
  warn "  inside a container is the container itself - so the node table and scenario"
  warn "  controls for a running simulation will be empty. Creating a fleet should work;"
  warn "  watching it from here does not yet. Tracked in SOW-0018."
  warn "For a fully working setup today, run this on Linux or in a Linux VM."
fi

# Resolve the daemon socket, rather than assuming /var/run/docker.sock: Docker
# Desktop's current default is ~/.docker/run/docker.sock with the /var/run symlink
# optional, which would have made this mount a non-existent path.
SOCK=""
ENDPOINT="$($DOCKER context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null || true)"
[ -z "$ENDPOINT" ] && ENDPOINT="${DOCKER_HOST:-}"
case "$ENDPOINT" in
  unix://*) SOCK="${ENDPOINT#unix://}" ;;
  "")       SOCK="/var/run/docker.sock" ;;
  *)
    die "docker endpoint '$ENDPOINT' is not a unix socket; container mode needs one to mount. Set DOCKER_HOST to a unix:// socket, or run on Linux."
    ;;
esac
[ -S "$SOCK" ] || die "docker socket '$SOCK' does not exist. Is Docker Desktop running?"
info "using docker socket $SOCK"

info "packaging the console for a non-Linux host"
run mkdir -p "$STATE_DIR"
CONSOLE_IMAGE="infra-sim-console:local"
# The builder first, unconditionally: the console image copies its binary from
# there, because `target/release` may hold a glibc or Mach-O build that cannot run
# in the image. Cached after the first run.
run $DOCKER build -f "$REPO/docker/builder.Dockerfile" -t "$BUILDER_IMAGE" "$REPO"
run $DOCKER build -f "$REPO/docker/console.Dockerfile" -t "$CONSOLE_IMAGE" "$REPO"

# Inside the container the console must listen on all interfaces, or the published
# port reaches nothing; the publish itself keeps it on the host's loopback.
port="${BIND##*:}"
host="${BIND%:*}"
[ "$host" = "$BIND" ] && host=127.0.0.1

info "starting the console on http://${host}:${port}"
# `-t` only when there is a terminal: piped from curl there is none, and docker
# refuses with "the input device is not a TTY".
TTY_FLAGS="-i"
[ -t 0 ] && TTY_FLAGS="-it"
# shellcheck disable=SC2086
exec $DOCKER run --rm $TTY_FLAGS \
  -v "$SOCK":/var/run/docker.sock \
  -v "$REPO":"$REPO" \
  -v "$STATE_DIR":"$STATE_DIR" \
  -e INFRA_SIM_STATE_DIR="$STATE_DIR" \
  -p "${host}:${port}:${port}" \
  -w "$REPO" \
  "$CONSOLE_IMAGE" --repo "$REPO" --bind "0.0.0.0:${port}" "${ENV_ARGS[@]+"${ENV_ARGS[@]}"}"
