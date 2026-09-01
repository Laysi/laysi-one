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

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 laysi

COPY --from=build /out/laysi-server /usr/local/bin/laysi-server
COPY --from=build /out/laysi-admin /usr/local/bin/laysi-admin

# Migrations and rulesets are already compiled into the binary (sqlx::migrate!
# and include_str!), so the runtime needs no sources.

USER laysi
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/laysi-server"]
