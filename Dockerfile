FROM rust:1.75 as build

# Create appuser.
# See https://stackoverflow.com/a/55757473/12429735
ENV USER=appuser
ENV UID=10001
RUN adduser \
    --disabled-password \
    --gecos "" \
    --home "/nonexistent" \
    --shell "/sbin/nologin" \
    --no-create-home \
    --uid "${UID}" \
    "${USER}"

RUN apt-get update && apt-get install -y ca-certificates

# Build. A statically linked musl binary stands in for CGO_ENABLED=0, so the
# final stage can stay on scratch as it does in the Go image.
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN rustup target add x86_64-unknown-linux-musl \
    && cargo build --release --target x86_64-unknown-linux-musl --bin hey \
    && cp target/x86_64-unknown-linux-musl/release/hey /usr/local/bin/hey

###############################################################################
# final stage
FROM scratch
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=build /etc/passwd /etc/passwd
COPY --from=build /etc/group /etc/group
USER appuser:appuser

ARG APPLICATION="hey"
ARG DESCRIPTION="HTTP load generator, ApacheBench (ab) replacement, formerly known as rakyll/boom"
ARG PACKAGE="rakyll/hey"

LABEL org.opencontainers.image.ref.name="${PACKAGE}" \
    org.opencontainers.image.authors="Jaana Dogan <@rakyll>" \
    org.opencontainers.image.documentation="https://github.com/${PACKAGE}/README.md" \
    org.opencontainers.image.description="${DESCRIPTION}" \
    org.opencontainers.image.licenses="Apache 2.0" \
    org.opencontainers.image.source="https://github.com/${PACKAGE}"

COPY --from=build /usr/local/bin/hey /hey
ENTRYPOINT ["/hey"]
