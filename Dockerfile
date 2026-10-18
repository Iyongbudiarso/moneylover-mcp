# Base images are pinned by digest (the tag is kept for readability); Dependabot
# (docker) proposes updates.
FROM rust:1.89-slim-bookworm@sha256:d7fc7de78bb8c1469933aeecbf801314d30d7d6e9f0578bba4cfa285bfa37fe6 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
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
