# A simulation's own agent.
#
# Netdata's stock image plus the two things a simulation needs and it does not
# ship: the correlated-logs receiver, and the plugin itself.
#
# The plugin is baked in rather than mounted so an image tag identifies exactly
# what a demo ran. Rebuild after changing the plugin.
# `latest` is netdata's nightly channel, and nightly is what simulations run.
# This is not a preference: the stable image's OpenTelemetry receiver has no
# `traces:` section in its otel.yaml and refuses spans outright, and it stores
# OTLP logs in an older journal-based layout. Nightly accepts traces and uses the
# WAL/SFST store. Pinning to stable silently removed a signal the simulator emits.
ARG NETDATA_TAG=latest
FROM netdata/netdata:${NETDATA_TAG}

# systemd-journal-remote is what lets a simulated node own its log source.
# journald refuses trusted fields such as _HOSTNAME from a local client, so
# without this every simulated node's logs attribute to the container.
# The stock image builds --internal-systemd-journal and ships the reader, not
# the receiver.
USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends systemd-journal-remote \
 && rm -rf /var/lib/apt/lists/*

# Built outside: `cargo build --release`, then `docker/build.sh`.
COPY infra-sim /etc/netdata/custom-plugins.d/infra-sim.plugin
RUN chmod 0755 /etc/netdata/custom-plugins.d/infra-sim.plugin

# Where a simulation's environment, specs and scenarios are mounted.
RUN mkdir -p /etc/netdata/infra-sim /var/log/journal/remote
