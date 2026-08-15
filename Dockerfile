# Fairgrounds Wait Times MCP — UGM 2026 demo
# Stateless Streamable HTTP for Railway / serverless.

FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache crate dependencies with a stub crate, then rebuild from real sources.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src \
    && rm -f target/release/deps/fairgrounds_wait_times_mcp* \
    && rm -f target/release/fairgrounds-wait-times-mcp

COPY src ./src
RUN touch src/main.rs \
    && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /app --shell /usr/sbin/nologin appuser
WORKDIR /app
COPY --from=builder --chown=appuser:appuser /app/target/release/fairgrounds-wait-times-mcp /app/fairgrounds-wait-times-mcp
USER appuser
ENV HOST=0.0.0.0
ENV PORT=8080
ENV MCP_TRANSPORT=http
EXPOSE 8080
CMD ["/app/fairgrounds-wait-times-mcp", "--http"]
