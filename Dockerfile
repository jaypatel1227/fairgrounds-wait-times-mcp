# Fairgrounds Wait Times MCP — UGM 2027 demo
# Stateless Streamable HTTP for Railway / serverless.

FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/fairgrounds-wait-times-mcp /app/fairgrounds-wait-times-mcp
ENV HOST=0.0.0.0
ENV PORT=8080
ENV MCP_TRANSPORT=http
EXPOSE 8080
CMD ["/app/fairgrounds-wait-times-mcp", "--http"]
