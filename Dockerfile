ARG PROFILE="debug"

FROM docker.io/library/rust:1.83-bookworm AS build

WORKDIR /work

COPY Cargo.toml .
COPY Cargo.lock .

ARG PROFILE

# RUN mkdir src \
#  && echo 'fn main() {}' > src/main.rs \
#  && if [ "$PROFILE" != "debug" ]; then cargo build "--$PROFILE"; else cargo build; fi \
#  && rm -R src

COPY src src

RUN if [ "$PROFILE" != "debug" ]; then cargo build "--$PROFILE"; else cargo build; fi

FROM docker.io/library/debian:bookworm-slim

ARG PROFILE

COPY --from=build "/work/target/${PROFILE}/redis-sentinel-service-controller" /redis-sentinel-service-controller

ENTRYPOINT [ "/redis-sentinel-service-controller" ]

