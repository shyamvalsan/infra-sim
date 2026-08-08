#!/usr/bin/env bash
#
# Run the OTLP showcase: an instrumented made-up service sending metrics, logs
# and traces straight into Netdata's OTLP receiver.
#
# Creates a local virtualenv on first run so nothing is installed system-wide.

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
VENV="${REPO_ROOT}/otlp/.venv"
ENDPOINT="${INFRA_SIM_OTLP_ENDPOINT:-http://127.0.0.1:4317}"

if [ ! -x "${VENV}/bin/python" ]; then
  echo -e >&2 "${GREEN}==>${NC} Creating ${VENV}"
  run python3 -m venv "${VENV}"
  run "${VENV}/bin/pip" install -q --disable-pip-version-check \
      -r "${REPO_ROOT}/otlp/requirements.txt"
fi

# Netdata listens on 127.0.0.1:4317 by default. A missing listener is the most
# common reason nothing appears, so say so before emitting into the void.
host_port="${ENDPOINT#*://}"
if ! ss -lnt 2>/dev/null | grep -q "${host_port##*:}"; then
  echo -e >&2 "${YELLOW}[warn]${NC} nothing is listening on ${host_port}."
  echo -e >&2 "       Netdata's OTLP receiver defaults to 127.0.0.1:4317."
  echo -e >&2 "       Check:  sudo grep -r otel /etc/netdata/netdata.conf"
fi

echo -e >&2 "${GREEN}==>${NC} Emitting OTLP to ${ENDPOINT}"
exec "${VENV}/bin/python" "${REPO_ROOT}/otlp/emit.py" --endpoint "${ENDPOINT}" "$@"
