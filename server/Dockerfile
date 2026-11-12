FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf src
COPY assets ./assets
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data dropcraft && mkdir -p /data && chown dropcraft /data
COPY --from=build /src/target/release/dropcraft-server /usr/local/bin/dropcraft-server
USER dropcraft
ENV DROPCRAFT_DATA=/data DROPCRAFT_BIND=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
CMD ["dropcraft-server"]
