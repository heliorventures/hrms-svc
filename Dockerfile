# syntax=docker/dockerfile:1.7

FROM rust:1.89-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --workspace --bins
RUN mkdir /out \
    && find target/release -maxdepth 1 -type f -executable -name 'kabipay-*' -exec cp {} /out/ \;

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /out/ /usr/local/bin/

ENV KABIPAY_LOG_FORMAT=json

EXPOSE 4001 4010 4013 4014 4015 4016 4017 4018 4019 4020 4021 4022 4023 4024 4025 4026 4027 4028 4029

CMD ["kabipay-auth"]
