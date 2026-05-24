# Builds chaser-cf (https://github.com/0xchasercat/chaser-cf) from source
# with the patches needed to run inside Compose:
#   * rust:1-bookworm — upstream pins 1.85 but `idna_adapter` needs 1.86+
#   * xvfb           — CF managed challenges detect pure-headless Chrome
#   * CHASER_VIRTUAL_DISPLAY=true, CHASER_EXTRA_ARGS=--no-sandbox
#
# Override the ref with --build-arg CHASER_CF_REF=<sha|tag|branch>.

ARG CHASER_CF_REF=main

FROM rust:1-bookworm AS builder
ARG CHASER_CF_REF
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config \
      libssl-dev \
      git \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN git clone --depth 1 --branch "${CHASER_CF_REF}" \
      https://github.com/0xchasercat/chaser-cf.git . \
    || (git clone https://github.com/0xchasercat/chaser-cf.git . \
        && git checkout "${CHASER_CF_REF}")

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --features http-server --bin chaser-cf-server \
    && cp /src/target/release/chaser-cf-server /usr/local/bin/chaser-cf-server

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

RUN useradd -m -s /bin/bash chaser

COPY --from=builder /usr/local/bin/chaser-cf-server /usr/local/bin/chaser-cf-server

ENV CHROME_BIN=/usr/bin/chromium \
    PORT=3000 \
    CHASER_CONTEXT_LIMIT=20 \
    CHASER_TIMEOUT=60000 \
    CHASER_PROFILE=windows \
    CHASER_VIRTUAL_DISPLAY=true \
    CHASER_EXTRA_ARGS=--no-sandbox

USER chaser
EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
  CMD curl -fsS http://127.0.0.1:3000/ > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["chaser-cf-server"]
