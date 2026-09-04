# Two stages: build against the full toolchain, ship the binary alone.
#
# The build context is the repository root, not this directory: the licence
# payload is one crate under backend/crates shared with the product, so a
# context scoped to license-server/ could not reach it.
FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY backend/crates/laysi-license-token ./backend/crates/laysi-license-token
COPY license-server/Cargo.toml license-server/Cargo.lock ./license-server/
COPY license-server/src ./license-server/src
COPY license-server/schema.sql ./license-server/schema.sql
WORKDIR /src/license-server
RUN cargo build --release --locked

FROM debian:trixie-slim
# The TLS stack is rustls, so only the certificate bundle is needed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin laysi
COPY --from=build /src/license-server/target/release/laysi-one-license /usr/local/bin/laysi-one-license
USER 10001
EXPOSE 8090
ENTRYPOINT ["/usr/local/bin/laysi-one-license"]
