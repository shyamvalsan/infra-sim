#!/usr/bin/env bash
#
# startsim-vm - run Infra-Sim on a Mac (or anywhere) via a Linux VM.
#
#   ./startsim-vm.sh
#
# This is the recommended route on macOS, and the reason is not preference: the
# console is built as a Linux binary and macOS cannot exec it, so `startsim.sh` there
# falls back to running the console in a container - which currently comes up with an
# empty node table (SOW-0018). Inside a VM, everything is the ordinary Linux path,
# which is the one that is fully exercised.
#
# What it touches:
#
#   * your machine - nothing. Multipass must already be installed; this says how if
#     it is not, and installs nothing itself.
#   * the VM it creates - freely. Docker and git are installed inside it, because
#     provisioning a machine we just created is the whole point of creating it. The
#     distinction is deliberate: your host is yours, the VM is ours.

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
    echo -e >&2 "${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    return $exit_code
  fi
}

info() { echo -e >&2 "${GREEN}==>${NC} $*"; }
warn() { echo -e >&2 "${YELLOW}[warn]${NC} $*"; }
die() { echo -e >&2 "${RED}[error]${NC} $*"; exit 1; }

VM="${INFRA_SIM_VM:-infra-sim}"
CPUS=4
MEMORY=8G
DISK=40G
PORT=19995
REPO_URL="${INFRA_SIM_REPO_URL:-https://github.com/shyamvalsan/infra-sim}"
RECREATE=no
SRC="/home/ubuntu/infra-sim"

usage() {
  cat >&2 <<'EOF'
usage: startsim-vm.sh [options]

  --name NAME      VM name (default infra-sim)
  --cpus N         default 4
  --memory SIZE    default 8G
  --disk SIZE      default 40G
  --port N         console port inside the VM (default 19995)
  --recreate       delete and rebuild the VM first
  --help

Size it for the fleet you intend: 160 simulated nodes is not a 2G job. The VM holds
its CPU and RAM for as long as it runs, and the simulations live inside it.

environment:
  INFRA_SIM_VM         VM name
  INFRA_SIM_REPO_URL   clone source
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --name) VM="${2:?--name needs a value}"; shift 2 ;;
    --cpus) CPUS="${2:?--cpus needs a value}"; shift 2 ;;
    --memory) MEMORY="${2:?--memory needs a value}"; shift 2 ;;
    --disk) DISK="${2:?--disk needs a value}"; shift 2 ;;
    --port) PORT="${2:?--port needs a value}"; shift 2 ;;
    --recreate) RECREATE=yes; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage; die "unknown option '$1'" ;;
  esac
done

# --- preflight: the host, which we do not modify ------------------------------

if ! command -v multipass >/dev/null 2>&1; then
  echo -e >&2 "${RED}[error]${NC} multipass is not installed, and this script will not install it for you."
  if [ "$(uname -s)" = Darwin ]; then
    echo -e >&2 "        install it: ${YELLOW}brew install --cask multipass${NC}"
  else
    echo -e >&2 "        install it: ${YELLOW}sudo snap install multipass${NC}"
    echo -e >&2 "        (on Linux you probably want ${YELLOW}./startsim.sh${NC} instead - no VM needed)"
  fi
  die "nothing was installed or changed."
fi

if [ "$(uname -s)" = Linux ]; then
  warn "You are on Linux, where ./startsim.sh runs natively and needs no VM."
  warn "Continuing anyway - a VM is still a reasonable way to isolate a fleet."
fi

exists() { multipass info "$VM" >/dev/null 2>&1; }

if [ "$RECREATE" = yes ] && exists; then
  info "deleting VM '$VM'"
  run multipass delete --purge "$VM"
fi

if exists; then
  info "reusing VM '$VM'"
  # A stopped VM is reused rather than recreated: it holds a fleet's history, and
  # GUIDs are identity - recreating would orphan whatever was running in it.
  state="$(multipass info "$VM" --format csv 2>/dev/null | awk -F, 'NR==2{print $2}')"
  if [ "$state" != "Running" ]; then
    info "starting it (was '$state')"
    run multipass start "$VM"
  fi
else
  info "launching VM '$VM' with ${CPUS} cpus, ${MEMORY} memory, ${DISK} disk"
  run multipass launch --name "$VM" --cpus "$CPUS" --memory "$MEMORY" --disk "$DISK"
fi

# --- provision the VM, which is ours ------------------------------------------

in_vm() { multipass exec "$VM" -- bash -lc "$1"; }

info "installing docker and git in the VM"
# docker.io from the distro rather than the convenience script: fewer moving parts,
# and the version is new enough for everything here.
run multipass exec "$VM" -- sudo bash -lc \
  'export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq docker.io git >/dev/null'

# No usermod or newgrp: startsim runs under sudo inside the VM, and root can always
# reach the daemon socket. Adding the ubuntu user to the docker group would need a new
# login session to take effect, which `multipass exec` does not give us.

if in_vm "test -d '$SRC/.git'"; then
  info "updating the checkout in the VM"
  in_vm "cd '$SRC' && git pull --ff-only" || warn "could not fast-forward; building what is there"
else
  info "cloning into the VM"
  run multipass exec "$VM" -- bash -lc "git clone '$REPO_URL' '$SRC'"
fi

IP="$(multipass info "$VM" --format csv 2>/dev/null | awk -F, 'NR==2{print $3}')"
[ -n "$IP" ] || die "could not read the VM's address from multipass info"

echo >&2
info "starting the console in the VM"
info "open:  ${YELLOW}http://${IP}:${PORT}${NC}"
echo -e >&2 "${GRAY}    Bound to 0.0.0.0 inside the VM, because you are reaching it from outside.${NC}"
echo -e >&2 "${GRAY}    That means it is on the VM's interface, not loopback - fine for a laptop VM,${NC}"
echo -e >&2 "${GRAY}    worth knowing if the VM is somewhere shared.${NC}"
echo -e >&2 "${GRAY}    Simulation dashboards appear on the same address, on their own ports.${NC}"
echo -e >&2 "${GRAY}    Ctrl-C stops the console; the VM and its simulations keep running.${NC}"
echo -e >&2 "${GRAY}    Later:  multipass stop ${VM}    /    multipass delete --purge ${VM}${NC}"
echo >&2

# Foreground, so Ctrl-C reaches it and the first run's build output is visible -
# the same shape as startsim.sh on Linux.
exec multipass exec "$VM" -- sudo bash -lc "cd '$SRC' && ./startsim.sh --bind 0.0.0.0:${PORT}"
