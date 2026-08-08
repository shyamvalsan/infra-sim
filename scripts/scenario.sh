#!/usr/bin/env bash
#
# Trigger and resolve Infra-Sim scenarios during a demo.
#
# Interim second-screen control until the console exists. Writes the control
# file the running plugin watches; the plugin picks changes up on its next tick,
# so effects begin within one collection interval.
#
# Usage:
#   scripts/scenario.sh list
#   scripts/scenario.sh status
#   scripts/scenario.sh trigger <name>
#   scripts/scenario.sh resolve <name>
#   scripts/scenario.sh resolve-all

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

INSTALL_DIR="${INFRA_SIM_DIR:-/etc/netdata/infra-sim}"
CONTROL="${INSTALL_DIR}/control.yaml"
SCENARIOS="${INSTALL_DIR}/scenarios"

usage() {
  sed -n '3,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-1}"
}

# Names currently listed as active in the control file.
current_active() {
  [ -f "${CONTROL}" ] || return 0
  grep -oE 'scenario:[[:space:]]*[A-Za-z0-9_.-]+' "${CONTROL}" 2>/dev/null \
    | awk '{print $2}' || true
}

# Existing start times, so rewriting the file never restarts a running scenario.
declare -A STARTED_AT
load_started_at() {
  [ -f "${CONTROL}" ] || return 0
  while read -r name ts; do
    [ -n "${name}" ] && [ -n "${ts}" ] && STARTED_AT["${name}"]="${ts}"
  done < <(grep -oE 'scenario:[[:space:]]*[A-Za-z0-9_.-]+,[[:space:]]*started_at:[[:space:]]*[0-9]+' \
             "${CONTROL}" 2>/dev/null \
           | sed -E 's/scenario:[[:space:]]*([A-Za-z0-9_.-]+),[[:space:]]*started_at:[[:space:]]*([0-9]+)/\1 \2/')
}
load_started_at

write_active() {
  local names=("$@")
  local tmp
  tmp="$(mktemp)"
  if [ ${#names[@]} -eq 0 ]; then
    printf 'active: []\n' > "${tmp}"
  else
    printf 'active:\n' > "${tmp}"
    for n in "${names[@]}"; do
      # A nameless entry has no start time to look up and would abort the whole
      # rewrite, so the fault would stay active instead of being resolved.
      [ -n "${n}" ] || continue
      # started_at is written explicitly and preserved across edits. Without it
      # the plugin assigns "now" on first read, so a plugin restart mid-demo
      # silently rewinds the scenario to its opening state - which looks exactly
      # like the fault resolving itself while the presenter is describing it.
      local started="${STARTED_AT[$n]:-$(date +%s)}"
      printf '  - { scenario: %s, started_at: %s }\n' "${n}" "${started}" >> "${tmp}"
    done
  fi
  # Written in place, not moved: /tmp is usually a different filesystem, and
  # only the control file itself is user-owned - the directory around it is
  # root's, so a rename would need privileges the SE should not be prompted for
  # mid-demo.
  if [ -w "${CONTROL}" ]; then
    cat "${tmp}" > "${CONTROL}"
  else
    run sudo install -m 0664 "${tmp}" "${CONTROL}"
  fi
  rm -f "${tmp}"
}

case "${1:-}" in
  list)
    if [ ! -d "${SCENARIOS}" ]; then
      echo -e >&2 "${RED}[ERROR]${NC} no scenario directory at ${SCENARIOS}"
      exit 1
    fi
    echo -e >&2 "${GREEN}Available scenarios:${NC}"
    for f in "${SCENARIOS}"/*.yaml; do
      [ -e "$f" ] || continue
      name="$(grep -m1 '^name:' "$f" | sed 's/^name:[[:space:]]*//')"
      root="$(grep -m1 '^  root_cause:' "$f" | sed 's/^  root_cause:[[:space:]]*//')"
      printf '  %-16s root cause: %s\n' "${name}" "${root}"
    done
    ;;

  status)
    echo -e >&2 "${GREEN}Active scenarios:${NC}"
    if [ ! -f "${CONTROL}" ]; then
      echo "  (control file absent - nothing running)"
    else
      mapfile -t active < <(current_active)
      if [ ${#active[@]} -eq 0 ]; then
        echo "  (none)"
      else
        printf '  %s\n' "${active[@]}"
      fi
    fi
    ;;

  trigger)
    [ $# -eq 2 ] || usage
    target="$2"
    if [ ! -f "${SCENARIOS}/${target}.yaml" ] && \
       ! grep -qlR "^name:[[:space:]]*${target}\$" "${SCENARIOS}" 2>/dev/null; then
      echo -e >&2 "${RED}[ERROR]${NC} unknown scenario '${target}'. Try: $0 list"
      exit 1
    fi
    mapfile -t active < <(current_active)
    for n in "${active[@]:-}"; do
      [ "$n" = "${target}" ] && { echo -e >&2 "${YELLOW}already running:${NC} ${target}"; exit 0; }
    done
    active+=("${target}")
    write_active "${active[@]}"
    echo -e >&2 "${GREEN}triggered:${NC} ${target}"
    echo -e >&2 "  effects begin within one collection interval"
    ;;

  resolve)
    [ $# -eq 2 ] || usage
    target="$2"
    mapfile -t active < <(current_active)
    remaining=()
    for n in ${active[@]+"${active[@]}"}; do
      [ -n "$n" ] && [ "$n" != "${target}" ] && remaining+=("$n")
    done
    # "${remaining[@]:-}" would expand an *empty* array to one empty string,
    # so resolving the last active scenario passed a nameless entry through and
    # aborted on a bad array subscript - leaving the fault running. This form
    # expands to nothing when the array is empty and is still safe under set -u.
    write_active ${remaining[@]+"${remaining[@]}"}
    echo -e >&2 "${GREEN}resolved:${NC} ${target}"
    ;;

  resolve-all)
    write_active
    echo -e >&2 "${GREEN}resolved all scenarios${NC}"
    ;;

  -h|--help|help) usage 0 ;;
  *) usage ;;
esac
