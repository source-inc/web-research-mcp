# syntax=docker/dockerfile:1.7
FROM rust:1.88-bookworm AS builder
ARG BINARY=web-research-mcp
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release --bin "${BINARY}" \
    && cp "target/release/${BINARY}" /usr/local/bin/service

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 service
COPY --from=builder /usr/local/bin/service /usr/local/bin/service
USER service
ENV WEB_RESEARCH_MCP_DATA_DIR=/home/service/.web-research-mcp/data
EXPOSE 9213
ENTRYPOINT ["/usr/local/bin/service"]
CMD ["serve"]
