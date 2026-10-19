# CPU image: ferraus plus a static FFmpeg so the bridge works offline.
FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p ferraus --bins

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates libmp3lame0 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/ferraus /usr/local/bin/ferraus
COPY --from=build /src/target/release/ferraus-plughost /usr/local/lib/ferraus/ferraus-plughost
ENV FERRAUS_HOME=/data
VOLUME ["/data", "/work"]
WORKDIR /work
ENTRYPOINT ["ferraus"]
CMD ["--help"]
