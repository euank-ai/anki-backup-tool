# Builder stage
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p anki-backup-daemon

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates sqlite3 libpq5 && \
    rm -rf /var/lib/apt/lists/*
RUN useradd -r -m -s /bin/false anki
COPY --from=builder /build/target/release/anki-backup-daemon /usr/local/bin/anki-backup-daemon
USER anki
WORKDIR /data
ENV ANKI_BACKUP_ROOT=/data
ENV ANKI_BACKUP_LISTEN=0.0.0.0:8088
EXPOSE 8088
CMD ["anki-backup-daemon"]
