# Base images are pinned by digest (the tag is kept for readability); Dependabot
# (docker) proposes updates.
FROM rust:1.98-slim-bookworm@sha256:ff521445a372125ed4f76e1453a1f8098f2d05332d1601d30db1c1f62757e730 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8
# The OS trust store is what the server checks Money Lover's certificates against.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 moneylover
COPY --from=build /src/target/release/moneylover-mcp /usr/local/bin/moneylover-mcp
USER moneylover
# Speaks MCP over stdio. Run `moneylover-mcp login` to store a token, or pass
# MONEYLOVER_EMAIL and MONEYLOVER_PASSWORD as secrets. Writes are off unless the
# container is started with MONEYLOVER_ALLOW_WRITE=true.
ENTRYPOINT ["moneylover-mcp"]
