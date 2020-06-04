FROM alpine:3.12.0 as builder

LABEL maintainer Robin Skahjem-Eriksen (skahjem-eriksen@stcorp.no)

RUN apk add --no-cache \
    cargo \
    openssl-dev

ADD . /tmp/build/

RUN cd /tmp/build \
    && cargo build --bins --release

FROM alpine:3.12.0
COPY --from=builder /tmp/build/target/release/scihub-query /usr/bin
RUN apk add --no-cache \
    openssl

ENTRYPOINT ["/usr/bin/scihub-query"]
