FROM rust:1.84.0-bookworm AS builder
COPY . /rustycan4docker
RUN cargo install --locked --path /rustycan4docker --root /rustycan4docker

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends iproute2 can-utils \
 && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /run/docker/plugins /var/lib/rustycan4docker/bpf
COPY --from=builder /rustycan4docker/bin/rustycan4docker /usr/local/bin/

CMD ["/usr/local/bin/rustycan4docker"]
