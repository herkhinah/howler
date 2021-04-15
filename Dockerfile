FROM docker.io/alpine:edge as builder
RUN apk add --no-cache \
    cargo \
    build-base \
    cmake \
    openssl-dev \
    olm-dev \
    git
COPY . /app
WORKDIR /app
RUN OLM_LINK_VARIANT=dylib cargo build --release

FROM docker.io/alpine:edge
RUN apk add --no-cache \
    openssl \
    olm \
  && mkdir -p /opt/howler
WORKDIR /opt/howler
COPY --from=builder /app/target/release/howler /usr/local/bin/howler
CMD ["/usr/local/bin/howler"]
