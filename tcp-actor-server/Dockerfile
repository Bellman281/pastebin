# Standalone image for the TCP actor server. Build context is THIS folder:
#   docker build -t tcp-actor-server .
#   docker run -p 8080:8080 -e APP_BIND_ADDR=0.0.0.0:8080 tcp-actor-server

FROM rust:1-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release --locked || cargo build --release

FROM debian:bookworm-slim
RUN useradd -r -u 10001 app
COPY --from=builder /app/target/release/tcp-actor-server /usr/local/bin/tcp-actor-server
USER app
# Bind to all interfaces inside the container.
ENV APP_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
CMD ["tcp-actor-server"]
