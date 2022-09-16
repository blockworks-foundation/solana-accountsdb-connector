# Base image containing all binaries, deployed to gcr.io/mango-markets/mango-services:latest
FROM rust:1.59 as build
ENV HOME=/home/root
ENV SCCACHE_CACHE_SIZE="8G"
ENV SCCACHE_DIR=$HOME/.cache/sccache
ENV RUSTC_WRAPPER="/usr/local/cargo/bin/sccache"

RUN apt-get update && apt-get -y install clang cmake
RUN cargo install sccache

WORKDIR $HOME/app
COPY ./ .

# Change this line to force docker recompilation from this step on.
# This will hit sccache the second time.
RUN echo 1

RUN --mount=type=cache,target=$HOME/.cache/sccache cargo build --release && sccache --show-stats
RUN mkdir .bin && cp target/release/service-mango-pnl target/release/service-mango-fills .bin/

FROM debian:bullseye-slim as run
RUN apt-get update && apt-get -y install ca-certificates libc6
COPY --from=build $HOME/app/.bin/* /usr/local/bin/