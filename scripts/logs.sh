#!/usr/bin/env bash
#
# Start, stop and inspect the correlated-logs writer.
#
# The writer is a separate process from the metrics plugin on purpose: Netdata
# owns the plugin's lifecycle, and this project has already lost time to a
# collector that outlived the removal of its own file. So this script tracks a
# PID and kills exactly that PID - never a pattern match, which on a machine
# running several simulations would stop somebody else's fleet.

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

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENVIRONMENT="${INFRA_SIM_ENVIRONMENT:-/etc/netdata/infra-sim/environment.yaml}"
JOURNAL_DIR="${INFRA_SIM_JOURNAL_DIR:-/var/log/journal/remote}"
PID_FILE="/run/infra-sim-logs.pid"
LOG_FILE="/var/log/infra-sim-logs.out"

binary() {
  for candidate in "${REPO_ROOT}/target/release/infra-sim" "${REPO_ROOT}/target/debug/infra-sim"; do
    if [ -x "${candidate}" ]; then
      echo "${candidate}"
      return 0
    fi
  done
  echo -e >&2 "${RED}[ERROR]${NC} No infra-sim binary. Build one first: cargo build --release"
  return 1
}

running_pid() {
  # Only a PID we recorded, and only if it is still the process we started.
  # Checked through /proc rather than `kill -0`: the writer runs as root, and
  # `kill -0` fails with EPERM for an unprivileged caller, which would report a
  # perfectly healthy writer as stopped and invite a second one alongside it.
  [ -f "${PID_FILE}" ] || return 1
  local pid
  pid="$(sudo cat "${PID_FILE}" 2>/dev/null || true)"
  [ -n "${pid}" ] || return 1
  [ -d "/proc/${pid}" ] || return 1
  # Guards against a recycled PID now belonging to something unrelated.
  grep -qa "infra-sim" "/proc/${pid}/cmdline" 2>/dev/null || return 1
  echo "${pid}"
}

cmd_start() {
  if pid="$(running_pid)"; then
    echo -e >&2 "${YELLOW}already running${NC} as PID ${pid}; stop it first"
    return 1
  fi

  local bin
  bin="$(binary)"

  if [ ! -r "${ENVIRONMENT}" ] && ! sudo test -r "${ENVIRONMENT}"; then
    echo -e >&2 "${RED}[ERROR]${NC} Cannot read environment '${ENVIRONMENT}'"
    echo -e >&2 "        Set INFRA_SIM_ENVIRONMENT to point at one."
    return 1
  fi

  echo -e >&2 "${GREEN}==>${NC} Starting the correlated-logs writer"
  echo -e >&2 "    environment: ${ENVIRONMENT}"
  echo -e >&2 "    journal dir: ${JOURNAL_DIR}"

  # Root is required to write under /var/log/journal. The PID recorded is the
  # writer itself, not sudo's, so stop kills the process that holds the pipes.
  #
  # The redirect goes *inside* the privileged shell: written outside it, the
  # calling shell opens the file as the invoking user and cannot write to
  # /var/log at all.
  #
  # The outer redirect matters too: sudo keeps this script's stdout and stderr
  # until it execs, so a backgrounded sudo holds the caller's pipe open and
  # anything reading this script's output (`| tail`, a CI step) hangs forever.
  sudo sh -c "echo \$\$ > '${PID_FILE}'; exec '${bin}' --logs \
      --environment '${ENVIRONMENT}' --journal-dir '${JOURNAL_DIR}' \
      >> '${LOG_FILE}' 2>&1" </dev/null >/dev/null 2>&1 &

  sleep 2
  if pid="$(running_pid)"; then
    echo -e >&2 "${GREEN}==>${NC} Running as PID ${pid}; output in ${LOG_FILE}"
    sudo tail -n 4 "${LOG_FILE}" >&2 || true
  else
    echo -e >&2 "${RED}[ERROR]${NC} Failed to start. Last output:"
    sudo tail -n 20 "${LOG_FILE}" >&2 || true
    return 1
  fi
}

cmd_stop() {
  if ! pid="$(running_pid)"; then
    echo -e >&2 "${YELLOW}not running${NC}"
    return 0
  fi
  echo -e >&2 "${GREEN}==>${NC} Stopping PID ${pid}"
  run sudo kill "${pid}"
  # Closing our pipes is what makes each systemd-journal-remote child finish
  # and exit, so no child is left writing to a journal nobody is feeding.
  for _ in 1 2 3 4 5; do
    running_pid >/dev/null || break
    sleep 1
  done
  if running_pid >/dev/null; then
    echo -e >&2 "${YELLOW}still running; sending SIGKILL${NC}"
    run sudo kill -9 "${pid}"
  fi
  sudo rm -f "${PID_FILE}"
  echo -e >&2 "${GREEN}==>${NC} Stopped"
}

cmd_status() {
  if pid="$(running_pid)"; then
    echo -e >&2 "${GREEN}running${NC} as PID ${pid}"
    echo -e >&2 "  journal-remote children: $(pgrep -c -P "${pid}" 2>/dev/null || echo 0)"
  else
    echo -e >&2 "${YELLOW}not running${NC}"
  fi
  echo -e >&2 "  journal files in ${JOURNAL_DIR}:"
  sudo ls -la "${JOURNAL_DIR}" 2>/dev/null | grep -E "remote-.*\.journal" >&2 || \
    echo -e >&2 "    (none)"
}

case "${1:-}" in
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  status) cmd_status ;;
  *)
    cat >&2 <<EOF
usage: $(basename "$0") {start|stop|status}

Writes correlated logs for the simulated fleet into the systemd journal, so
each node appears as its own log source in Netdata and fault lines follow
whatever scenario is running.

  start   begin writing (needs root, and systemd-journal-remote installed)
  stop    stop writing and let every journal-remote child finish
  status  show the writer and the journal files it owns

environment:
  INFRA_SIM_ENVIRONMENT  environment.yaml to read
                         (default: /etc/netdata/infra-sim/environment.yaml)
  INFRA_SIM_JOURNAL_DIR  where journal files are written
                         (default: /var/log/journal/remote)
EOF
    exit 1
    ;;
esac
