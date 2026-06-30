# syntax=docker/dockerfile:1
# Standalone image for the pastebin service. Build context is THIS folder:
#   docker build -t pastebin-service .
# (The `static/` web client is embedded into the binary at compile time via
#  include_str!, so it ships inside the executable — no runtime asset dir.)

# ---- builder ----------------------------------------------------------------
FROM rust:1.82 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

# ---- runtime ----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 app \
 && mkdir -p /data && chown app:app /data

COPY --from=builder /app/target/release/pastebin-service /usr/local/bin/pastebin-service

USER app
ENV APP_BIND_ADDR=0.0.0.0:8090 \
    DATABASE_URL=sqlite:///data/pastes.db \
    DATABASE_MAX_CONNECTIONS=5 \
    PUBLIC_BASE_URL=http://localhost:8090 \
    REQUEST_TIMEOUT_SECS=10 \
    MAX_CONCURRENT_REQUESTS=1024 \
    RUST_LOG=info

EXPOSE 8090
VOLUME ["/data"]
HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
  CMD curl -fsS http://localhost:8090/health/ready || exit 1
CMD ["pastebin-service"]
