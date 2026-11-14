# Multi-stage build of the strata binaries. Documented, not exercised in
# this repository's benchmarks (no Docker daemon was used; see docs/DEPLOY.md).
#
#   docker build -t strata .
#   docker run -p 7700:7700 -v strata-data:/data strata strata-server --dir /data --http 0.0.0.0:7700
FROM rust:1.94-bookworm AS build
WORKDIR /src
COPY . .
# protox compiles the .proto in pure Rust: no protoc needed in the image.
RUN cargo build --release -p strata-server -p strata-raft \
    && strip target/release/strata-server target/release/strata-coord \
             target/release/strata-load target/release/strata-mcp \
             target/release/strata-raft-node

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 strata && mkdir /data && chown strata /data
COPY --from=build /src/target/release/strata-server /src/target/release/strata-coord \
     /src/target/release/strata-load /src/target/release/strata-mcp \
     /src/target/release/strata-raft-node /usr/local/bin/
USER strata
VOLUME /data
EXPOSE 7700 7701
ENTRYPOINT []
CMD ["strata-server", "--dir", "/data", "--http", "0.0.0.0:7700", "--grpc", "0.0.0.0:7701"]
