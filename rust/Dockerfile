# ghmcp en service réseau : `ghmcp serve` (MCP HTTP + webhooks + API d'activité).
# docker build -t ghmcp . && docker run -p 8080:8080 -v ghmcp-data:/data -e GHMCP_API_TOKEN=... ghmcp
# Multi-arch (release) : docker buildx build --platform linux/amd64,linux/arm64 . — compilation croisée
# sur la machine de build (aucune émulation), la suite de tests tourne dans l'instance amd64.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS build
ARG TARGETARCH=amd64
WORKDIR /src
RUN if [ "$TARGETARCH" = arm64 ]; then \
      apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
      && rm -rf /var/lib/apt/lists/* && rustup target add aarch64-unknown-linux-gnu; fi
COPY . .
# Porte de prod : la suite complète passe avant le binaire ; un test rouge = pas d'image, donc pas de livraison.
RUN set -e; \
    if [ "$TARGETARCH" = arm64 ]; then \
      T=aarch64-unknown-linux-gnu; \
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
    else \
      T=x86_64-unknown-linux-gnu; cargo test --workspace --locked; \
    fi; \
    cargo build --release --locked -p ghmcp --target "$T" && cp "target/$T/release/ghmcp" /ghmcp && mkdir -p /vide/data

# Binaires seuls (assets de la GitHub Release) : docker buildx build --target binaires --output type=local,dest=dist .
FROM scratch AS binaires
COPY --from=build /ghmcp /ghmcp

# distroless/cc : glibc + certificats, aucun RUN à émuler pour arm64. UID 10001 = celui des volumes existants.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /ghmcp /usr/local/bin/ghmcp
COPY --from=build --chown=10001:10001 /vide/data /data
USER 10001
VOLUME /data
EXPOSE 8080
# /config/config.toml est monté par l'hôte ; il doit définir [webhook] api_token_env (refus d'écouter sur 0.0.0.0 sinon).
ENTRYPOINT ["ghmcp", "serve", "--http", "0.0.0.0:8080", "--config", "/config/config.toml", "--db", "/data/activity.db"]
