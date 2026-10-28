# syntax=docker/dockerfile:1
# The Linux desktop app's .deb and .rpm, built on Ubuntu 24.04 like the
# release (its GTK 4.14 and libadwaita 1.5 are the oldest the app supports).
# Used by `make dist-linux-app-x86_64` / `make dist-linux-app-arm64`:
#   docker buildx build --platform linux/arm64 --build-arg TARGET=aarch64-unknown-linux-gnu \
#     -f packaging/linux/Dockerfile.app --output dist .

FROM ubuntu:24.04 AS build
ARG TARGET
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        appstream build-essential ca-certificates curl libadwaita-1-dev libgtk-4-dev perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.99 \
    && cargo install --locked cargo-deb cargo-generate-rpm
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/target,id=capycal-app-$TARGET \
    cargo build --release --locked --features gtk --bin capycal-app --target "$TARGET" \
    && packaging/linux/build-app-packages.sh "$TARGET" \
    && mkdir /out && cp dist/capycal-app* /out/

FROM scratch
COPY --from=build /out/ /
