#!/usr/bin/env bash
#
# Install the Infra-Sim host-monitoring pack onto THIS machine's Netdata agent.
# Run by whoever owns the hosted box - it is not run by the repo, and never
# against a developer's personal agent without their say-so.
#
#   host-monitoring.sh install    install/refresh the alert pack, enable docker charts
#   host-monitoring.sh verify     show what is installed and whether the agent took it
#
# Idempotent: safe to re-run after upgrades.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONF=infra-sim-host.conf
DEST_DIR=/etc/netdata/health.d

say() { printf '%s\n' "$*"; }

cmd_install() {
  [ -d "$DEST_DIR" ] || { echo "no $DEST_DIR - is a Netdata agent installed here?"; exit 1; }
  install -m 0644 "$REPO/monitoring/$CONF" "$DEST_DIR/$CONF"
  say "installed $DEST_DIR/$CONF"

  # Docker charts: the agent ships the collector but does not enable it by
  # default. Only created when absent - an operator's own docker.conf is
  # theirs.
  if [ ! -f /etc/netdata/go.d/docker.conf ]; then
    # Stock template location differs by install: the netdata image keeps it
    # under /usr/lib/netdata/conf.d, package installs sometimes under
    # /etc/netdata/go.d/conf.d. Either is a valid source.
    for stock in /usr/lib/netdata/conf.d/go.d/docker.conf /etc/netdata/go.d/conf.d/docker.conf; do
      if [ -f "$stock" ]; then
        cp "$stock" /etc/netdata/go.d/docker.conf
        say "enabled docker charts (go.d/docker.conf from $stock)"
        break
      fi
    done
    [ -f /etc/netdata/go.d/docker.conf ] || say "no stock docker.conf template found - docker charts not enabled"
  else
    say "docker charts already configured - left untouched"
  fi

  netdatacli reload-health >/dev/null 2>&1 \
    && say "health config reloaded" \
    || say "could not reload health (is netdatacli in PATH? try: netdatacli reload-health)"
}

cmd_verify() {
  [ -f "$DEST_DIR/$CONF" ] && say "alert pack: installed ($DEST_DIR/$CONF)" \
                            || say "alert pack: NOT installed"
  [ -f /etc/netdata/go.d/docker.conf ] && say "docker charts: configured" \
                                       || say "docker charts: not configured"
  say "agent alarms named infra_sim:"
  curl -s --max-time 5 "http://127.0.0.1:19999/api/v1/alarms?all" 2>/dev/null \
    | grep -o '"infra_sim[^"]*"' | sort -u || say "  (agent not reachable on 19999)"
}

case "${1:-}" in
  install) cmd_install ;;
  verify)  cmd_verify ;;
  *) sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
