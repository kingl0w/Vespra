FROM rust:1.85-slim AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/vespra-keymaster /usr/local/bin/
EXPOSE 9100
ENV VESPRA_KM_HOST=0.0.0.0
ENV VESPRA_KM_PORT=9100
ENV VESPRA_KM_DB=/opt/vespra-keymaster/keymaster.db
ENV VESPRA_KM_KILL_SWITCH_STATE=/opt/vespra-keymaster/kill-switch.state
VOLUME ["/opt/vespra-keymaster"]
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:9100/health || exit 1
CMD ["vespra-keymaster"]
