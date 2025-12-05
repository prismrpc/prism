# =============================================================================
# Prism RPC Aggregator - Production Dockerfile
# Multi-stage build for minimal image size (~80-120MB)
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: Builder - Compile Rust application
# -----------------------------------------------------------------------------
FROM rustlang/rust:nightly-bookworm AS builder

# Install build dependencies (no OpenSSL needed - using rustls)
RUN apt-get update && apt-get install -y \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/

# Fetch dependencies (cached unless Cargo.toml changes)
RUN cargo fetch

# Build release binaries
# Profile settings from workspace Cargo.toml:
#   - opt-level = 3, lto = true, codegen-units = 1, strip = true
RUN cargo build --release --bin server --bin cli

# -----------------------------------------------------------------------------
# Stage 2: Runtime - Minimal Debian image
# -----------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
# - ca-certificates: For HTTPS upstream connections (rustls uses system certs)
# - curl: For health checks
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN groupadd -r prism && useradd -r -g prism -u 1000 prism

# Create application directories
RUN mkdir -p /app/config /app/db /app/logs && \
    chown -R prism:prism /app

WORKDIR /app

# Copy binaries from builder
COPY --from=builder /build/target/release/server /usr/local/bin/prism-server
COPY --from=builder /build/target/release/cli /usr/local/bin/prism-cli

# Copy default configuration
COPY config/config.toml /app/config/config.toml

# Set ownership
RUN chown prism:prism /usr/local/bin/prism-server /usr/local/bin/prism-cli

# Switch to non-root user
USER prism

# Environment configuration
ENV PRISM_CONFIG=/app/config/config.toml \
    RUST_LOG=info,prism_core=info,server=info

# Expose ports
# 3030: Main RPC endpoint
# 9090: Prometheus metrics
EXPOSE 3030 9090

# Health check using the /health endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -sf http://localhost:3030/health || exit 1

# Run the server
ENTRYPOINT ["/usr/local/bin/prism-server"]

# Build metadata
LABEL org.opencontainers.image.title="Prism RPC Aggregator" \
      org.opencontainers.image.description="High-performance Ethereum RPC aggregator with intelligent caching and load balancing" \
      org.opencontainers.image.version="0.1.0" \
      org.opencontainers.image.vendor="Prism" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.source="https://github.com/prismrpc/prism"
