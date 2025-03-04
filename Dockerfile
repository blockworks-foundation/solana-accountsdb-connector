# syntax = docker/dockerfile:1.2
# Base image containing all binaries, deployed to gcr.io/mango-markets/mango-geyser-services:latest
FROM rust:1.59.0 as base
RUN cargo install cargo-chef
RUN rustup component add rustfmt
RUN apt-get update && apt-get install -y clang cmake
WORKDIR /app

FROM base AS plan
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base as build
COPY --from=plan /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin service-mango-fills --bin service-mango-pnl

FROM debian:bullseye-slim as run
RUN apt-get update && apt-get -y install ca-certificates libc6
COPY --from=build /app/target/release/service-mango-* /usr/local/bin/
COPY --from=build /app/service-mango-pnl/template-config.toml ./pnl-config.toml
COPY --from=build /app/service-mango-fills/template-config.toml ./fills-config.toml