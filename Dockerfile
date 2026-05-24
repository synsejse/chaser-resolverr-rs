FROM rust:1-bookworm AS base
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --version ^0.1

FROM base AS planner
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef prepare --recipe-path recipe.json

FROM base AS builder
WORKDIR /app
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin chaser-resolverr-rs

# Bundles Chrome + Xvfb in the runtime image — the browser is driven from
# in-process via the vendored chaser-cf library, not a sidecar.
FROM debian:bookworm-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      chromium \
      chromium-sandbox \
      fonts-liberation \
      libasound2 \
      libatk-bridge2.0-0 \
      libatk1.0-0 \
      libatspi2.0-0 \
      libcups2 \
      libdbus-1-3 \
      libdrm2 \
      libgbm1 \
      libgtk-3-0 \
      libnspr4 \
      libnss3 \
      libwayland-client0 \
      libxcomposite1 \
      libxdamage1 \
      libxfixes3 \
      libxkbcommon0 \
      libxrandr2 \
      xdg-utils \
      ca-certificates \
      xvfb \
      curl \
      dumb-init \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/chaser-resolverr-rs /usr/local/bin/chaser-resolverr-rs

EXPOSE 8191

RUN mkdir -p /data
VOLUME ["/data"]

# chaser-cf is *not* headless — Cloudflare detects headless Chrome at the
# binary level, so we run against an Xvfb virtual display.
ENV CHROME_BIN=/usr/bin/chromium \
    CHASER_VIRTUAL_DISPLAY=true \
    CHASER_HEADLESS=false \
    CHASER_EXTRA_ARGS=--no-sandbox \
    CHASER_CONTEXT_LIMIT=20 \
    CHASER_TIMEOUT=60000 \
    CHASER_PROFILE=windows \
    RUST_LOG=info,tracing::span=warn

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8191/health > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["/usr/local/bin/chaser-resolverr-rs"]
