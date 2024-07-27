FROM rust:1-alpine as build

RUN rustup target add x86_64-unknown-linux-musl && \
    mkdir /app

WORKDIR /app
COPY . /app

RUN cargo build --release && strip target/release/init

FROM tianon/toybox:0

COPY --from=build /app/target/release/init /init

ENTRYPOINT [ "/init" ]