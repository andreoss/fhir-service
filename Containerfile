ARG TOOLCHAIN=1.97.1
ARG RUNTIME=12-slim

FROM docker.io/library/rust:${TOOLCHAIN}-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
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
USER served
EXPOSE ${PORT}
ENTRYPOINT ["/usr/local/bin/fhir-host"]
