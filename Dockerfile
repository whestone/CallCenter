# Ein Container mit App + eigener PostgreSQL – gedacht für die öffentliche Demo.
# Ohne DATABASE_URL startet eine frische Datenbank mit Demodaten bei jedem Start.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY templates templates
COPY migrations migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends postgresql ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/callcenter /app/callcenter
COPY static static
COPY deploy/entrypoint.sh /app/entrypoint.sh
RUN chmod +x /app/entrypoint.sh
ENV RUST_LOG=callcenter=info
EXPOSE 3000
CMD ["/app/entrypoint.sh"]
