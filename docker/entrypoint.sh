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

# If netdata dies, so does the container (docker --restart then revives the
# whole supervised set - the point of this wrapper).
wait "$NETDATA_PID"
kill "$WATCHDOG_PID" 2>/dev/null || true
