# syntax=docker/dockerfile:1
# Multi-stage builder — produces a minimal unisonfs runtime image.
#
# Build:
#   docker build -t unisonfs .
#
# Run (FUSE on Linux):
#   docker run --rm --device /dev/fuse --cap-add SYS_ADMIN \
#     -e UNISON_TOKEN=usk_live_... \
#     -v /mnt/brain:/mnt/brain:shared \
#     unisonfs mount /mnt/brain --foreground

# ── Stage 1: builder ──────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /build

# Install FUSE dev headers (needed for fuser on Linux)
RUN apt-get update -qq && apt-get install -y --no-install-recommends \
        libfuse-dev fuse3 \
    && rm -rf /var/lib/apt/lists/*

# Cache dependencies: copy manifests first, then src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/

RUN cargo build --release --bin unisonfs 2>&1

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Runtime deps: FUSE, CA certificates for HTTPS
RUN apt-get update -qq && apt-get install -y --no-install-recommends \
        fuse3 libfuse2 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Allow non-root FUSE mounts inside the container (user_allow_other)
RUN echo "user_allow_other" >> /etc/fuse.conf

COPY --from=builder /build/target/release/unisonfs /usr/local/bin/unisonfs

# Create a non-root user for the process
RUN useradd -m -u 1000 unisonfs
USER unisonfs

ENV UNISON_TOKEN=""
ENV UNISON_API_URL=""

ENTRYPOINT ["/usr/local/bin/unisonfs"]
CMD ["--help"]
