#!/usr/bin/env bash
#
# Install an Infra-Sim environment into a local Netdata agent for verification.
#
# The agent rescans its plugin directories on the interval set by
# [plugins]."check for new plugins every" (60s by default), so no restart is
# needed to pick up a newly installed plugin.
#
# Usage:
#   scripts/install-local.sh [ENVIRONMENT_YAML]
#
# Default environment: environments/web-stack.yaml

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
ENVIRONMENT="${1:-${REPO_ROOT}/environments/web-stack.yaml}"

NETDATA_CONFIG_DIR="${NETDATA_CONFIG_DIR:-/etc/netdata}"
PLUGIN_DIR="${NETDATA_CONFIG_DIR}/custom-plugins.d"
INSTALL_DIR="${NETDATA_CONFIG_DIR}/infra-sim"
BINARY="${REPO_ROOT}/target/release/infra-sim"

if [ ! -f "${ENVIRONMENT}" ]; then
  echo -e >&2 "${RED}[ERROR]${NC} environment not found: ${ENVIRONMENT}"
  exit 1
fi

echo -e >&2 "${GREEN}==>${NC} Building release binary"
run cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"

# Fail before touching the agent if the specs would emit clamped, flattened
# metrics. 72h matches the ML warm-up window the demo runbook requires.
echo -e >&2 "${GREEN}==>${NC} Running fidelity lint (72 simulated hours)"
run "${BINARY}" --environment "${ENVIRONMENT}" --lint 72

echo -e >&2 "${GREEN}==>${NC} Installing to ${INSTALL_DIR}"
run sudo mkdir -p "${INSTALL_DIR}/specs" "${PLUGIN_DIR}"

# The generator path inside environment.yaml is resolved relative to the
# environment file, so it is rewritten to match the installed layout.
GENERATOR_REL="$(grep -E '^generator:' "${ENVIRONMENT}" | head -1 | sed -E 's/^generator:[[:space:]]*//')"
GENERATOR_SRC="$(cd "$(dirname "${ENVIRONMENT}")" && cd "$(dirname "${GENERATOR_REL}")" && pwd)/$(basename "${GENERATOR_REL}")"

if [ ! -f "${GENERATOR_SRC}" ]; then
  echo -e >&2 "${RED}[ERROR]${NC} generator spec not found: ${GENERATOR_SRC}"
  exit 1
fi

run sudo install -m 0644 "${GENERATOR_SRC}" "${INSTALL_DIR}/specs/$(basename "${GENERATOR_SRC}")"

TMP_ENV="$(mktemp)"
trap 'rm -f "${TMP_ENV}"' EXIT
sed -E -e "s|^generator:.*|generator: specs/$(basename "${GENERATOR_SRC}")|" \
       -e "s|^scenarios:.*|scenarios: scenarios|" \
  "${ENVIRONMENT}" > "${TMP_ENV}"
run sudo install -m 0644 "${TMP_ENV}" "${INSTALL_DIR}/environment.yaml"

# Scenarios are looked up relative to the environment file.
SCENARIO_SRC="$(cd "$(dirname "${ENVIRONMENT}")" && cd "$(dirname "$(grep -E '^scenarios:' "${ENVIRONMENT}" | head -1 | sed -E 's/^scenarios:[[:space:]]*//')")" && pwd)/$(basename "$(grep -E '^scenarios:' "${ENVIRONMENT}" | head -1 | sed -E 's/^scenarios:[[:space:]]*//')")"
if [ -d "${SCENARIO_SRC}" ]; then
  run sudo mkdir -p "${INSTALL_DIR}/scenarios"
  for f in "${SCENARIO_SRC}"/*.yaml; do
    [ -e "$f" ] || continue
    run sudo install -m 0644 "$f" "${INSTALL_DIR}/scenarios/$(basename "$f")"
  done
fi

# The control file is how scenarios are triggered live. Created empty (no
# scenarios running) and left writable by the invoking user so triggering during
# a demo does not need a privilege prompt at the worst possible moment.
if [ ! -f "${INSTALL_DIR}/control.yaml" ]; then
  printf 'active: []\n' | run sudo tee "${INSTALL_DIR}/control.yaml" >/dev/null
fi
run sudo chown "$(id -u):$(id -g)" "${INSTALL_DIR}/control.yaml"

# Netdata only runs files it recognises as plugins.
run sudo install -m 0755 "${BINARY}" "${PLUGIN_DIR}/infra-sim.plugin"

# Replacing the file does NOT replace the running plugin: `install` writes a new
# inode and the existing process keeps executing the old image, so an upgrade
# silently does nothing. Netdata restarts a plugin that exits, so killing the
# old process is what actually deploys the new binary.
#
# Matched on the exact installed path, never a bare process name, so this cannot
# touch an unrelated process.
OLD_PIDS="$(pgrep -x -f "${PLUGIN_DIR}/infra-sim.plugin.*" 2>/dev/null || \
            pgrep -f "^${PLUGIN_DIR}/infra-sim\.plugin( |$)" 2>/dev/null || true)"
if [ -n "${OLD_PIDS}" ]; then
  echo -e >&2 "${GREEN}==>${NC} Stopping previous plugin process(es) so Netdata restarts the new binary"
  for pid in ${OLD_PIDS}; do
    run sudo kill "${pid}"
  done
else
  echo -e >&2 "${GREEN}==>${NC} No previous plugin process running"
fi

echo -e >&2 "${GREEN}==>${NC} Installed."
echo -e >&2 "    plugin:      ${PLUGIN_DIR}/infra-sim.plugin"
echo -e >&2 "    environment: ${INSTALL_DIR}/environment.yaml"
echo -e >&2 "    generator:   ${INSTALL_DIR}/specs/$(basename "${GENERATOR_SRC}")"
echo -e >&2 "    scenarios:   ${INSTALL_DIR}/scenarios/"
echo -e >&2 "    control:     ${INSTALL_DIR}/control.yaml"
echo -e >&2 ""
echo -e >&2 "    The agent rescans for new plugins every 60s. Then check:"
echo -e >&2 "      curl -s localhost:19999/api/v3/nodes | grep sim-"
echo -e >&2 ""
echo -e >&2 "    To remove:"
echo -e >&2 "      sudo rm -rf ${PLUGIN_DIR}/infra-sim.plugin ${INSTALL_DIR}"
echo -e >&2 "      sudo systemctl restart netdata"
