# Image for vibescan, the vibenet block explorer.
#
# Build from repo root:
#   docker build -f etc/vibenet/explorer/Dockerfile -t vibescan:local .
#
# Mirrors the mold + cargo cache pattern used by etc/vibenet/faucet/Dockerfile
# so incremental rebuilds share a target cache with the other vibenet rust
# images.

ARG RUST_VERSION=1.93
FROM public.ecr.aws/docker/library/rust:${RUST_VERSION}-trixie AS builder-base
WORKDIR /app

ARG MOLD_VERSION=2.40.4
ARG MOLD_SHA256_AARCH64=c799b9ccae8728793da2186718fbe53b76400a9da396184fac0c64aa3298ec37
ARG MOLD_SHA256_ARM=d82792748a81202423ddd2496fc8719404fe694493abdef691cc080392ee44bf
ARG MOLD_SHA256_X86_64=4c999e19ffa31afa5aa429c679b665d5e2ca5a6b6832ad4b79668e8dcf3d8ec1

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      git libclang-dev pkg-config curl build-essential cmake && \
    rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    case "$(uname -m)" in \
      x86_64) MOLD_ARCH=x86_64; MOLD_SHA256="${MOLD_SHA256_X86_64}" ;; \
      aarch64|arm64) MOLD_ARCH=aarch64; MOLD_SHA256="${MOLD_SHA256_AARCH64}" ;; \
      armv7l|armv6l) MOLD_ARCH=arm; MOLD_SHA256="${MOLD_SHA256_ARM}" ;; \
      *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-${MOLD_ARCH}-linux.tar.gz" -o /tmp/mold.tar.gz; \
    echo "${MOLD_SHA256}  /tmp/mold.tar.gz" | sha256sum -c -; \
    tar -xzf /tmp/mold.tar.gz -C /tmp; \
    cp /tmp/mold-${MOLD_VERSION}-${MOLD_ARCH}-linux/bin/* /usr/local/bin/; \
    rm -rf /tmp/mold*

FROM builder-base AS vibescan-builder
ARG PROFILE=release
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/app/target,id=vibenet-vibescan-target,sharing=locked \
    cargo build --profile "$PROFILE" --package base-vibenet-explorer --bin vibescan && \
    cp /app/target/$([ "$PROFILE" = "dev" ] && echo debug || echo "$PROFILE")/vibescan /app/vibescan

FROM public.ecr.aws/docker/library/debian:trixie-slim AS runtime
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=vibescan-builder /app/vibescan /app/vibescan

# Mount / create at /data; indexer DB lives here so it survives restarts.
VOLUME ["/data"]

EXPOSE 8080
ENTRYPOINT ["/app/vibescan"]
