FROM rust:1.96.1-slim-bookworm AS builder

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
COPY --chown=root:root THIRD_PARTY_NOTICES.md THIRD_PARTY_LICENSES.md /usr/share/doc/ycloud/

WORKDIR /var/lib/ycloud
ENV BIND_ADDRESS=0.0.0.0 \
    PORT=18473 \
    STORAGE_PATH=/var/lib/ycloud/storage \
    CONFIG_PATH=/var/lib/ycloud/config.json \
    RUST_LOG=info

EXPOSE 18473
# Runtime identity is selected by docker run --user or Compose user.
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/ycloud"]
