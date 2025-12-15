FROM rust:1.92.0-alpine3.23 AS builder

RUN set -eux; \
    apk add --no-cache musl-dev openssl-dev perl make lld; \
    rustup target add x86_64-unknown-linux-musl

WORKDIR /opt/app

COPY LICENSE /opt/app/LICENSE
COPY Cargo.toml /opt/app/Cargo.toml
COPY Cargo.lock /opt/app/Cargo.lock

RUN mkdir -p /opt/app/src && echo "fn main() {}" > /opt/app/src/main.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/opt/app/target \
    set -eux; \
    cargo fetch --locked --target x86_64-unknown-linux-musl

RUN rm -f /opt/app/src/main.rs
COPY src/ /opt/app/src/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/opt/app/target \
    set -eux; \
    export RUSTFLAGS="-C linker=lld"; \
    cargo build --locked --release --target x86_64-unknown-linux-musl; \
    cp /opt/app/target/x86_64-unknown-linux-musl/release/tino /opt/app/tino


FROM scratch AS runtime

COPY --from=builder /opt/app/tino /sbin/tino

ENTRYPOINT ["/sbin/tino"]
