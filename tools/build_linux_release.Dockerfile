# Pinned multi-architecture Rust 1.95.0 / Debian 12 build environment.
FROM rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS build
RUN apt-get update && apt-get install -y --no-install-recommends libclang-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
ENV RUSTFLAGS="--remap-path-prefix=/src=/source --remap-path-prefix=/usr/local/cargo=/cargo"
RUN --network=none cargo build --release --locked --offline --package meeting-call-pilot && \
    mkdir /out && cp target/release/meeting-call-pilot /out/meeting-call-pilot && \
    strip /out/meeting-call-pilot && \
    ldd /out/meeting-call-pilot > /out/libraries.txt && \
    objdump -T /out/meeting-call-pilot > /out/symbols.txt

FROM scratch
COPY --from=build /out/ /
