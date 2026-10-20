# syntax=docker/dockerfile:1.7

FROM rust:1.89-bookworm AS builder

WORKDIR /app

ARG CARGO_BUILD_JOBS=1

RUN apt-get update \
    && apt-get install -y --no-install-recommends jq \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --locked --jobs ${CARGO_BUILD_JOBS} --release --workspace --bins \
        --message-format=json-render-diagnostics > /tmp/cargo-artifacts.jsonl \
    && mkdir /out \
    && jq -r 'select(.reason == "compiler-artifact") | select(.executable != null) | select(.target.kind | index("bin")) | select(.target.name | (startswith("kabipay-") or . == "bootstrap_organization")) | .executable' \
        /tmp/cargo-artifacts.jsonl > /tmp/cargo-executables \
    && test -s /tmp/cargo-executables \
    && while IFS= read -r artifact; do \
        test "$(dirname "$artifact")" = /app/target/release || exit 1; \
        name="$(basename "$artifact")"; \
        case "$name" in kabipay-*|bootstrap_organization) ;; *) exit 1 ;; esac; \
        test -x "$artifact" && cp "$artifact" "/out/$name" || exit 1; \
    done < /tmp/cargo-executables \
    && test -x /out/bootstrap_organization \
    && test -x /out/kabipay-organization \
    && test -x /out/kabipay-auth

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /out/ /usr/local/bin/

ENV KABIPAY_LOG_FORMAT=json

EXPOSE 4001 4010 4013 4014 4015 4016 4017 4018 4019 4020 4021 4022 4023 4024 4025 4026 4027 4028 4029 4030 4031

CMD ["kabipay-auth"]
