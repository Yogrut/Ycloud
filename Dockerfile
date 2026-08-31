FROM rust:1.98.0-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

ARG YCLOUD_UID=10001
ARG YCLOUD_GID=10001

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid "${YCLOUD_GID}" ycloud \
    && useradd --uid "${YCLOUD_UID}" --gid "${YCLOUD_GID}" --no-create-home --home-dir /var/lib/ycloud --shell /usr/sbin/nologin ycloud \
    && install --directory --owner=ycloud --group=ycloud --mode=0700 /var/lib/ycloud /var/lib/ycloud/storage

COPY --from=builder --chown=root:root /build/target/release/ycloud /usr/local/bin/ycloud

WORKDIR /var/lib/ycloud
ENV BIND_ADDRESS=0.0.0.0 \
    PORT=18473 \
    STORAGE_PATH=/var/lib/ycloud/storage \
    CONFIG_PATH=/var/lib/ycloud/config.json \
    RUST_LOG=info

EXPOSE 18473
USER 10001:10001
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/ycloud"]
