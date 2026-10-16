FROM rust:1.89-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
# The OS trust store is what the server checks Money Lover's certificates against.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 moneylover
COPY --from=build /src/target/release/moneylover-mcp /usr/local/bin/moneylover-mcp
USER moneylover
# Speaks MCP over stdio. Run `moneylover-mcp login` to store a token, or pass
# MONEYLOVER_EMAIL and MONEYLOVER_PASSWORD as secrets. Writes stay off unless
# MONEYLOVER_ALLOW_WRITE=true.
ENTRYPOINT ["moneylover-mcp"]
