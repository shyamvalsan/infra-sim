# The console, packaged to run on a host that cannot execute Linux binaries.
#
# On macOS the binaries built by `builder.Dockerfile` are Linux ELF and the host
# cannot exec them - a real failure, several minutes into a build. Docker Desktop
# runs Linux containers happily, so the console runs in one and drives the host's
# Docker through the mounted socket. Sibling containers, not nested: the daemon is
# the host's.
#
# No Rust change was needed for this, which is the reason it is worth doing. The
# console already shells out to `bash scripts/sim-docker.sh` for build and create,
# and already honours `INFRA_SIM_STATE_DIR`, so packaging it is a delivery concern
# rather than a port.
#
# The repository and the state directory are mounted at **identical paths** inside
# and out. That is load-bearing: `sim-docker.sh` passes `-v <path>:...` to the
# daemon, and the daemon resolves those against the *host* filesystem. Mount them
# anywhere else and the simulation containers would silently bind empty
# directories.
FROM docker:cli

# bash because sim-docker.sh is bash, not sh; python3 because it rewrites the
# scenario control file and reads the agent's node list. Both are checked by
# startsim's preflight on Linux and must exist here too.
RUN apk add --no-cache bash python3

# Taken from the builder image rather than from `target/release`, and that matters:
# a later `cargo build --release` replaces the static musl binaries with
# glibc-linked ones, which cannot run on Alpine at all ("no such file or
# directory", because the ELF interpreter is missing - observed, not theorised).
# On macOS `target/release` could hold a Mach-O binary, which is worse. The builder
# is the only source guaranteed to be static Linux.
COPY --from=infra-sim-builder:local /out/infra-sim-console /usr/local/bin/infra-sim-console
RUN chmod 0755 /usr/local/bin/infra-sim-console

# 0.0.0.0 rather than 127.0.0.1: inside the container, loopback is the container's
# own and the published port would reach nothing. The port publish is what keeps
# it bound to the host's loopback.
EXPOSE 19995
ENTRYPOINT ["/usr/local/bin/infra-sim-console"]
