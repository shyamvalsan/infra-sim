# A simulation's own agent.
#
# Netdata's stock image plus the two things a simulation needs and it does not
# ship: the correlated-logs receiver, and the plugin itself.
#
# The plugin is baked in rather than mounted so an image tag identifies exactly
# what a demo ran. Rebuild after changing the plugin.
ARG NETDATA_TAG=stable
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
