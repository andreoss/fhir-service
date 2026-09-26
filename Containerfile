ARG TOOLCHAIN=1.97.1
ARG RUNTIME=12-slim

FROM docker.io/library/rust:${TOOLCHAIN}-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY store ./store
COPY contract ./contract
COPY adapter-memory ./adapter-memory
COPY adapter-document ./adapter-document
COPY adapter-relational ./adapter-relational
COPY jobs ./jobs
COPY telemetry ./telemetry
COPY api ./api
COPY host ./host
COPY shadow ./shadow
COPY tools ./tools
RUN cargo build --workspace --release --locked

FROM docker.io/library/debian:${RUNTIME} AS service
ARG PORT=8080
ENV FHIR_BIND=0.0.0.0:${PORT}
WORKDIR /srv
RUN useradd --system --no-create-home --uid 10001 served
COPY --from=build /src/target/release/fhir-host /usr/local/bin/fhir-host
COPY --from=build /src/target/release/apply /usr/local/bin/apply
COPY --from=build /src/target/release/load /usr/local/bin/load
COPY --from=build /src/target/release/dump /usr/local/bin/dump
COPY --from=build /src/target/release/reindex /usr/local/bin/reindex
COPY --from=build /src/target/release/generate /usr/local/bin/generate
COPY --from=build /src/target/release/measure /usr/local/bin/measure
COPY --from=build /src/target/release/scaffold /usr/local/bin/scaffold
COPY --from=build /src/target/release/instances /usr/local/bin/instances
COPY --from=build /src/target/release/probe /usr/local/bin/probe
ENV FHIR_ADDRESS_FILE=/srv/address
RUN install -d -o served -g served /srv
USER served
EXPOSE ${PORT}

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD /usr/local/bin/probe "$(cat "${FHIR_ADDRESS_FILE}" 2>/dev/null || echo "${FHIR_BIND}")" /health
ENTRYPOINT ["/usr/local/bin/fhir-host"]
