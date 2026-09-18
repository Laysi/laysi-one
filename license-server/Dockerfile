# Two stages: build against the full toolchain, ship the binary alone.
#
# The build context is the repository root, not this directory: the licence
# payload is one crate under backend/crates shared with the product, so a
# context scoped to license-server/ could not reach it.

# The build stage stays on the build machine's own architecture and cross-links
# for the target, the same arrangement backend/Dockerfile uses. Emulating it
# would run a release build of this crate under QEMU; only the runtime stage
# below is emulated, and all it does is install a CA bundle.
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

COPY backend/crates/laysi-license-token ./backend/crates/laysi-license-token
COPY license-server/Cargo.toml license-server/Cargo.lock ./license-server/
COPY license-server/src ./license-server/src
COPY license-server/schema.sql ./license-server/schema.sql
WORKDIR /src/license-server

# A cache mount is not part of the image, so the binary has to be copied out of
# `target` inside the same RUN; after it ends the mount is gone. `target` is
# keyed by architecture: a multi-platform build runs the two platforms
# concurrently, and a shared mount would make one cargo wait on the other's
# lock for the whole build.
RUN --mount=type=cache,id=laysi-license-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=laysi-license-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=laysi-license-target-$TARGETARCH,target=/src/license-server/target \
    cargo build --release --locked --target "$(cat /triple)" \
    && mkdir -p /out \
    && cp "target/$(cat /triple)/release/laysi-one-license" /out/

FROM debian:trixie-slim
# The TLS stack is rustls, so only the certificate bundle is needed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin laysi
COPY --from=build /out/laysi-one-license /usr/local/bin/laysi-one-license
USER 10001
EXPOSE 8090
ENTRYPOINT ["/usr/local/bin/laysi-one-license"]
