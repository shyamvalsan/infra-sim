# Builds the two Infra-Sim binaries without a Rust toolchain on the host.
#
# Docker is already mandatory here - a simulation *is* a container - so building
# in it removes the only dependency that did not have to be installed. This is
# what lets `startsim.sh` work on a machine that has nothing but Docker.
#
# Alpine, so the output is statically linked. That is not a preference either:
# the plugin runs inside the simulation container (Debian, glibc 2.41 at the time
# of writing) while the console runs on the host (glibc 2.43 here, older on
# plenty of machines). A dynamically linked build would quietly constrain which
# machines each artifact works on; a static one cannot.
#
# The tag is pinned so the compiler cannot change under an operator between runs.
# 1.88 is the real floor - `tonic 0.14.6` in Cargo.lock requires it - despite
# `Cargo.toml` still declaring `rust-version = "1.85"`, which fails to build.
ARG RUST_TAG=1.97-alpine
FROM rust:${RUST_TAG} AS build

# musl-dev provides the C runtime bits the linker needs for a static target.
RUN apk add --no-cache musl-dev

WORKDIR /src

# Manifests first, so dependency compilation caches across source-only changes.
COPY Cargo.toml Cargo.lock ./
COPY crates crates

RUN cargo build --release --bin infra-sim --bin infra-sim-console

# A payload stage rather than an image that runs anything: `startsim.sh` creates a
# container from this only to `docker cp` the binaries out. Copying out beats a
# bind mount, which would write root-owned artifacts straight into the operator's
# checkout.
FROM scratch
COPY --from=build /src/target/release/infra-sim /out/infra-sim
COPY --from=build /src/target/release/infra-sim-console /out/infra-sim-console

# Never executed. `docker create` refuses an image with no command ("no command
# specified"), and a container is what `docker cp` needs to copy from. Declaring
# one keeps this working on any Docker, rather than requiring the BuildKit-only
# `docker build -o` export.
CMD ["/out/infra-sim", "--help"]
