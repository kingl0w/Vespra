FROM debian:bookworm-slim AS builder
RUN apt-get update && apt-get install -y curl xz-utils && rm -rf /var/lib/apt/lists/*
RUN curl -fL https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz | tar -xJ
ENV PATH="/zig-x86_64-linux-0.15.2:$PATH"
WORKDIR /app
COPY . .
RUN zig build -Doptimize=ReleaseSafe

FROM debian:bookworm-slim
COPY --from=builder /app/zig-out/bin/nullboiler /usr/local/bin/
VOLUME ["/data"]
EXPOSE 9090
CMD ["nullboiler", "--port", "9090", "--db", "/data/nullboiler.db"]
