# Two stages: build against the full toolchain, ship the binary alone.
FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY schema.sql ./schema.sql
RUN cargo build --release --locked

FROM debian:trixie-slim
# The TLS stack is rustls, so only the certificate bundle is needed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin laysi
COPY --from=build /src/target/release/laysi-one-license /usr/local/bin/laysi-one-license
USER 10001
EXPOSE 8090
ENTRYPOINT ["/usr/local/bin/laysi-one-license"]
