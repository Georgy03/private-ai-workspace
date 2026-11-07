# syntax=docker/dockerfile:1
# The cloud service: the API, the worker and the file reader, in one image. Railway runs it twice
# with different start commands (railway.api.toml and railway.worker.toml). Build context: backend/.
#
#   docker build -t workspace-cloud backend/
#   docker run --rm workspace-cloud            # the API (needs the settings in README.md)
#   docker run --rm workspace-cloud worker     # the worker
#   docker run --rm workspace-cloud migrate    # migrations; normally run with scripts/neon-migrate.sh instead

FROM rust:1-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake clang pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Dependencies first, so a change to the code does not compile them all again.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
 && echo 'fn main() {}' > src/main.rs \
 && : > src/lib.rs \
 && echo 'fn main() {}' > src/bin/workspace-extract.rs \
 && cargo build --release --locked

COPY src ./src
COPY migrations ./migrations
# Files copied in keep their old dates, so cargo is told they are new.
RUN touch src/main.rs src/lib.rs src/bin/workspace-extract.rs \
 && cargo build --release --locked

# The meaning-search model (about 130 MB) is fetched now and shipped in the image. The API loads it
# before it listens, so fetching it on first start would make the first start slow and fragile.
ENV EMBEDDING_CACHE_DIR=/opt/models
RUN ./target/release/workspace-cloud download-model

# The programs the service shells out to: OCR for scans and photos (Tesseract), and Poppler to turn
# PDF pages into pictures first. Without them those files are found by name only.
FROM debian:bookworm-slim AS runtime-base
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tesseract-ocr tesseract-ocr-eng poppler-utils libstdc++6 libgomp1 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin app

FROM runtime-base AS runtime
# The file reader must sit beside the service program: the worker looks for it there.
COPY --from=build /src/target/release/workspace-cloud /src/target/release/workspace-extract /usr/local/bin/
COPY --from=build --chown=app:app /opt/models /opt/models
# DATABASE_CA_FILE is the system's certificate bundle, which holds Neon's authority. The service
# refuses to reach the database without it, and never skips checking the database's certificate.
ENV EMBEDDING_CACHE_DIR=/opt/models \
    DATABASE_CA_FILE=/etc/ssl/certs/ca-certificates.crt \
    HOME=/tmp \
    RUST_LOG=info
USER app
EXPOSE 8080
ENTRYPOINT ["workspace-cloud"]
