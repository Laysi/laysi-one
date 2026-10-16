# The build context is the repository root rather than backend/: the workspace
# manifest points license-file at ../LICENSE.txt, so the licence has to be
# reachable inside the context.

# The build stage stays on the build machine's own architecture and cross-links
# for the target. Emulating it instead would run a release build of this
# workspace — thin LTO, one codegen unit — under QEMU, which takes hours; only
# the runtime stage below is emulated, and all it does is install a CA bundle.
FROM --platform=$BUILDPLATFORM rust:1-slim-trixie AS build
WORKDIR /src

ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && if [ "$TARGETARCH" = arm64 ]; then \
           apt-get install -y --no-install-recommends \
               gcc-aarch64-linux-gnu libc6-dev-arm64-cross; \
       fi \
    && rm -rf /var/lib/apt/lists/*

# TARGETARCH is Docker's name for the architecture; cargo wants a target triple.
RUN case "$TARGETARCH" in \
        amd64) echo x86_64-unknown-linux-gnu ;; \
        arm64) echo aarch64-unknown-linux-gnu ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac > /triple \
    && rustup target add "$(cat /triple)"

# ring, blake3 and argon2 compile C sources, so the cross compiler has to be
# named twice: once for cargo, which links, and once for cc, which compiles
# them. Both are ignored on amd64, where nothing is cross compiled.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc

# The licence is a manifest input.
COPY LICENSE.txt ./LICENSE.txt
COPY backend ./backend

WORKDIR /src/backend

# The sources and the build share one layer, so editing one line of Rust
# invalidates it. Without these cache mounts that means re-fetching the
# registry and recompiling every dependency from scratch on every build, which
# is most of the wall-clock time.
#
# A cache mount is not part of the image, so the binaries have to be copied out
# of `target` inside the same RUN; after it ends the mount is gone. Keeping
# `target` on a cache mount also stops it from bloating the layer, which is why
# the copy goes to /out rather than leaving the whole directory behind.
#
# `target` is keyed by architecture: a multi-platform build runs the two
# platforms concurrently, and a shared cache mount would make one cargo wait on
# the other's lock for the whole build.

# What the running system reports as its version. release.yml passes the tag
# being built; a plain `docker build` leaves it empty and the binary falls back
# to the crate version. Declared here rather than at the top of the stage so it
# invalidates nothing above it.
ARG LAYSI_BUILD

RUN --mount=type=cache,id=laysi-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=laysi-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=laysi-target-release-$TARGETARCH,target=/src/backend/target \
    cargo build --release --target "$(cat /triple)" --bin laysi-server --bin laysi-admin \
    && mkdir -p /out \
    && cp "target/$(cat /triple)/release/laysi-server" \
          "target/$(cat /triple)/release/laysi-admin" /out/

# The host-side updater, which travels in this image so that the release
# being installed brings the next one (backend/crates/laysi-updater). It runs
# on the host, not in a container, so it is a static musl binary: linked by
# rust-lld against the musl the Rust target ships, with no C toolchain and
# no glibc to match on the host. Its own stage, so it builds alongside the
# server and invalidates nothing there.
FROM --platform=$BUILDPLATFORM rust:1-slim-trixie AS updater
WORKDIR /src
ARG TARGETARCH
RUN case "$TARGETARCH" in \
        amd64) echo x86_64-unknown-linux-musl ;; \
        arm64) echo aarch64-unknown-linux-musl ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac > /triple \
    && rustup target add "$(cat /triple)"
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
COPY LICENSE.txt ./LICENSE.txt
COPY backend ./backend
WORKDIR /src/backend
ARG LAYSI_BUILD
RUN --mount=type=cache,id=laysi-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=laysi-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=laysi-target-updater-$TARGETARCH,target=/src/backend/target \
    cargo build --release --locked --target "$(cat /triple)" -p laysi-updater --bin laysi-updater \
    && mkdir -p /out \
    && cp "target/$(cat /triple)/release/laysi-updater" /out/

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 laysi

COPY --from=build /out/laysi-server /usr/local/bin/laysi-server
COPY --from=build /out/laysi-admin /usr/local/bin/laysi-admin

# What the host-side updater takes out of this image when it installs this
# release: the compose files the release runs with and the updater that
# will handle the next one. The paths are fixed in
# backend/crates/laysi-updater/src/layout.rs.
COPY docker-compose.prod.yml docker-compose.ship.yml /usr/local/share/laysi-one/
COPY --from=updater /out/laysi-updater /usr/local/share/laysi-one/laysi-updater

# Migrations and rulesets are already compiled into the binary (sqlx::migrate!
# and include_str!), so the runtime needs no sources.

USER laysi
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/laysi-server"]
