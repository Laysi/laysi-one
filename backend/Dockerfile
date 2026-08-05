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
RUN cargo build --release --bin laysi-server --bin laysi-admin

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 laysi

COPY --from=build /src/backend/target/release/laysi-server /usr/local/bin/laysi-server
COPY --from=build /src/backend/target/release/laysi-admin /usr/local/bin/laysi-admin

# Migrations and rulesets are already compiled into the binary (sqlx::migrate!
# and include_str!), so the runtime needs no sources. Brand integrity therefore
# verifies which sources this binary was built from, rather than whatever files
# happen to sit in the container.

USER laysi
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/laysi-server"]
