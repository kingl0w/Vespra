FROM debian:bookworm-slim AS builder
RUN apt-get update && apt-get install -y curl xz-utils unzip && rm -rf /var/lib/apt/lists/*
RUN curl -fL https://ziglang.org/download/0.15.2/zig-x86_64-linux-0.15.2.tar.xz | tar -xJ
ENV PATH="/zig-x86_64-linux-0.15.2:$PATH"
WORKDIR /app
COPY . .
# SQLite amalgamation is gitignored (large vendored C source) — fetch at build time.
RUN curl -fL https://www.sqlite.org/2024/sqlite-amalgamation-3450000.zip \
      -o sqlite.zip && \
    unzip -q sqlite.zip && \
    mkdir -p deps/sqlite && \
    cp sqlite-amalgamation-3450000/sqlite3.h deps/sqlite/ && \
    cp sqlite-amalgamation-3450000/sqlite3.c deps/sqlite/ && \
    cp sqlite-amalgamation-3450000/sqlite3ext.h deps/sqlite/ && \
    rm -rf sqlite.zip sqlite-amalgamation-3450000
RUN zig build -Doptimize=ReleaseSafe

FROM debian:bookworm-slim
COPY --from=builder /app/zig-out/bin/nullboiler /usr/local/bin/
VOLUME ["/data"]
EXPOSE 9090
CMD ["nullboiler", "--port", "9090", "--db", "/data/nullboiler.db"]
