#!/bin/sh
#
# Supervised entrypoint for a simulation container.
#
# The stock image's run.sh starts netdata and nothing else; the telemetry
# side-processes (correlated logs, OTLP emitter, Prometheus exporters) used to
# be `docker exec -d` one-shots at create time - which meant a container
# restart (crash, host reboot, docker daemon upgrade) revived the agent but
# silently lost every one of them for good (review finding: the always-on
# hosted demo lost logs, traces and exporter charts after any restart).
#
# This wrapper keeps netdata's own launcher exactly as the stock image runs
# it, and adds a watchdog that (re)starts the telemetry processes when the
# plugin binary is present and telemetry has not been deliberately stopped.
# Deliberate stops win: `sim-docker.sh telemetry <name> stop` drops a marker
# into the payload directory, and the watchdog respects it until `start`
# removes the marker again.

set -eu

PLUGIN=/etc/netdata/custom-plugins.d/infra-sim.plugin
ENV=/etc/netdata/infra-sim/environment.yaml
PAYLOAD=/etc/netdata/infra-sim
OFF_MARKER="$PAYLOAD/.telemetry-off"
EXPORTERS_MARKER="$PAYLOAD/.exporters-on"
INTERVAL=15

# All producer UIDs share only this simulation's recording directory.
if [ -n "${INFRA_SIM_RECORD_DIR:-}" ]; then
  install -d -o netdata -g netdata -m 2770 "$INFRA_SIM_RECORD_DIR"
fi

if [ -n "${INFRA_SIM_LIFECYCLE_FILE:-}" ]; then
  printf 'running\n' > "$INFRA_SIM_LIFECYCLE_FILE"
fi

# netdata first, as the stock image would run it.
/usr/sbin/run.sh &
NETDATA_PID=$!

watchdog() {
  while :; do
    sleep "$INTERVAL"
    # No plugin mounted yet (or ever): nothing to supervise.
    [ -f "$PLUGIN" ] || continue
    # An operator stopped telemetry on purpose; `start` clears this.
    [ -f "$OFF_MARKER" ] && continue
    if ! pgrep -f "^$PLUGIN --logs" >/dev/null 2>&1; then
      "$PLUGIN" --logs --environment "$ENV" >>/tmp/infra-sim-logs.log 2>&1 &
    fi
    if ! pgrep -f "^$PLUGIN --otlp" >/dev/null 2>&1; then
      "$PLUGIN" --otlp --environment "$ENV" >>/tmp/infra-sim-otlp.log 2>&1 &
    fi
    # Exporters are conditional at create time; the marker carries that intent
    # across restarts (docker labels cannot: they are immutable).
    if [ -f "$EXPORTERS_MARKER" ] \
      && ! pgrep -f "^$PLUGIN --exporters" >/dev/null 2>&1; then
      "$PLUGIN" --exporters --environment "$ENV" >>/tmp/infra-sim-exporters.log 2>&1 &
    fi
  done
}

watchdog &
WATCHDOG_PID=$!

# Docker signals PID 1. Forward shutdown to this container's exact plugin
# executables and wait for their recording buffers before the namespace exits.
shutdown() {
  trap '' TERM INT
  # Netdata can respawn a collector during the flush window. New invocations
  # must exit before opening another producer session or writing a handshake.
  if [ -n "${INFRA_SIM_LIFECYCLE_FILE:-}" ]; then
    printf 'stopping\n' > "$INFRA_SIM_LIFECYCLE_FILE"
  fi
  kill "$WATCHDOG_PID" 2>/dev/null || true
  plugin_pids=""
  for command_line in /proc/[0-9]*/cmdline; do
    # Different producer UIDs can make /proc/PID/exe unreadable in Docker.
    # Match the complete argv[0], never a generic process name or substring.
    [ "$(tr '\000' '\n' < "$command_line" 2>/dev/null | sed -n '1p')" = "$PLUGIN" ] || continue
    pid="${command_line#/proc/}"
    pid="${pid%/cmdline}"
    plugin_pids="$plugin_pids $pid"
    kill -TERM "$pid" 2>/dev/null || true
  done
  attempts=0
  while [ "$attempts" -lt 20 ]; do
    alive=no
    for pid in $plugin_pids; do
      if [ -r "/proc/$pid/cmdline" ] && [ "$(tr '\000' '\n' < "/proc/$pid/cmdline" 2>/dev/null | sed -n '1p')" = "$PLUGIN" ]; then alive=yes; fi
    done
    [ "$alive" = yes ] || break
    sleep 1
    attempts=$((attempts + 1))
  done
  kill -TERM "$NETDATA_PID" 2>/dev/null || true
  wait "$NETDATA_PID" 2>/dev/null || true
  exit 0
}
trap shutdown TERM INT

# A dead agent ends the container; Docker's restart policy revives the fleet.
wait "$NETDATA_PID" || true
shutdown
