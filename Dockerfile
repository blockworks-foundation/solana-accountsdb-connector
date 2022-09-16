# syntax = docker/dockerfile:1.2
# Base image containing all binaries, deployed to gcr.io/mango-markets/mango-services:latest
FROM rust:1.59 as build
ENV HOME=/home/root
ENV SCCACHE_CACHE_SIZE="8G"
ENV SCCACHE_DIR=$HOME/.cache/sccache
ENV RUSTC_WRAPPER="/usr/local/bin/sccache"

RUN apt-get update && apt-get -y install clang cmake
RUN wget https://github.com/mozilla/sccache/releases/download/v0.2.15/sccache-v0.2.15-x86_64-unknown-linux-musl.tar.gz \
    && tar xzf sccache-v0.2.15-x86_64-unknown-linux-musl.tar.gz \
    && mv sccache-v0.2.15-x86_64-unknown-linux-musl/sccache /usr/local/bin/sccache \
    && chmod +x /usr/local/bin/sccache
RUN rustup component add rustfmt

WORKDIR $HOME/app
COPY ./ .

RUN --mount=type=cache,target=$HOME/.cache/sccache cargo build --release
RUN mkdir .bin && cp target/release/service-mango-pnl target/release/service-mango-fills .bin/

FROM debian:bullseye-slim as run
RUN apt-get update && apt-get -y install ca-certificates libc6
COPY --from=build /home/root/app/.bin/* /usr/local/bin/