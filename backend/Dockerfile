# The build context is the repository root rather than backend/: brand integrity
# compiles LICENSE-BSL.txt into the binary with include_str!, so the licence has
# to be reachable inside the context.

FROM rust:1-slim-bookworm AS build
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*

# The licence, the brand manifest and the public key are compile-time inputs.
COPY LICENSE-BSL.txt ./LICENSE-BSL.txt
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
RUN --mount=type=cache,id=laysi-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=laysi-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=laysi-target-release,target=/src/backend/target \
    cargo build --release --bin laysi-server --bin laysi-admin \
    && mkdir -p /out \
    && cp target/release/laysi-server target/release/laysi-admin /out/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 laysi

COPY --from=build /out/laysi-server /usr/local/bin/laysi-server
COPY --from=build /out/laysi-admin /usr/local/bin/laysi-admin

# Migrations and rulesets are already compiled into the binary (sqlx::migrate!
# and include_str!), so the runtime needs no sources. Brand integrity therefore
# verifies which sources this binary was built from, rather than whatever files
# happen to sit in the container.

USER laysi
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/laysi-server"]
