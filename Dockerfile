# OpenFML budget server — one container, one port: UI + API.
#   docker build -t openfml .
#   docker run -p 8080:8080 -v openfml-data:/data openfml
# /data is the config directory and holds ALL state (users.cfg,
# access.cfg, models/, logs/ signed audit chains, server.secret) —
# it survives upgrades; the image is stateless. Upgrade = pull the new
# image, restart with the same volume.

FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN rustup target add wasm32-unknown-unknown \
 && cargo build --release \
 && cargo build --release --target wasm32-unknown-unknown

FROM debian:bookworm-slim
COPY --from=build /src/target/release/openfml /src/target/release/openfml-server /usr/local/bin/
COPY www /opt/openfml/www
COPY --from=build /src/target/wasm32-unknown-unknown/release/openfml.wasm /opt/openfml/www/openfml.wasm
COPY deploy-template /opt/openfml/deploy-template
WORKDIR /opt/openfml
EXPOSE 8080
VOLUME /data
# First run seeds /data from the template; later runs leave it alone.
CMD ["sh", "-c", "if [ ! -f /data/users.cfg ]; then cp -R /opt/openfml/deploy-template/. /data/; fi; cp -R /opt/openfml/www /data/www; exec openfml-server /data 8080"]
