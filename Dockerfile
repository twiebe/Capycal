FROM rust:1.99-slim-trixie AS builder

WORKDIR /build

# Cache dependency builds: copy manifests first, build a dummy, then copy source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release && rm -rf src

# Build the real binary
COPY src/ src/
RUN touch src/main.rs && cargo build --release

# ---

FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --system --create-home capycal

COPY --from=builder /build/target/release/capycal /usr/local/bin/capycal

RUN mkdir -p /etc/capycal /var/lib/capycal \
    && chown capycal:capycal /var/lib/capycal

USER capycal

# Where docker-compose.yml mounts the state volume; state_dir in the config wins
ENV CAPYCAL_STATE_DIR=/var/lib/capycal

ENTRYPOINT ["capycal"]
CMD ["--config", "/etc/capycal/capycal.toml"]
