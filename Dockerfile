# syntax=docker/dockerfile:1
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release && cp target/release/feedme /feedme

FROM scratch

LABEL org.opencontainers.image.title="feedme"
LABEL org.opencontainers.image.description="A Fever API compatible RSS feed aggregator"
LABEL org.opencontainers.image.source="https://github.com/moritzheiber/feedme"
LABEL org.opencontainers.image.url="https://github.com/moritzheiber/feedme"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.vendor="Moritz Heiber"

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /feedme /feedme

EXPOSE 8080

ENTRYPOINT ["/feedme"]
CMD ["serve"]
