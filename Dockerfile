FROM rust:1.98-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY contracts ./contracts
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --create-home --home-dir /home/app app \
    && install -d -o app -g app -m 0700 /var/lib/workspace-mcp/credentials

COPY --from=builder /build/target/release/workspace-mcp-rs /usr/local/bin/workspace-mcp-rs

ENV HOME=/home/app \
    WORKSPACE_MCP_HOST=0.0.0.0 \
    WORKSPACE_MCP_PORT=8000 \
    WORKSPACE_MCP_CREDENTIALS_DIR=/var/lib/workspace-mcp/credentials

USER app
WORKDIR /var/lib/workspace-mcp
EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=10s --start-period=10s --retries=3 \
    CMD sh -c 'curl --fail --silent --show-error "http://127.0.0.1:${WORKSPACE_MCP_PORT:-8000}/health" >/dev/null'

ENTRYPOINT ["workspace-mcp-rs"]
CMD ["--transport", "streamable-http"]
