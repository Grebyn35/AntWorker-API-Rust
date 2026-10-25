# syntax=docker/dockerfile:1.27.1@sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e

# The API's image (OPS-15). `just image` builds it for linux/amd64 and linux/arm64 (OPS-14):
# the binary for both is compiled once, on the platform of the machine that builds, and each
# platform's image copies its own. docs/operations/release.md describes the image.

# zig is the C compiler and the linker for both targets, for the C and assembly in ring.
FROM --platform=$BUILDPLATFORM alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a AS zig-amd64
ADD --checksum=sha256:70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz /zig.tar.xz
RUN mkdir /zig && tar -xJf /zig.tar.xz -C /zig --strip-components=1

FROM --platform=$BUILDPLATFORM alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a AS zig-arm64
ADD --checksum=sha256:ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17 https://ziglang.org/download/0.16.0/zig-aarch64-linux-0.16.0.tar.xz /zig.tar.xz
RUN mkdir /zig && tar -xJf /zig.tar.xz -C /zig --strip-components=1

FROM zig-$BUILDARCH AS zig

# rustup installs the toolchain that rust-toolchain.toml pins, whichever one the image holds
# (CODE-01). cargo-zigbuild builds with zig, cargo-auditable records the crates in the binary for
# the scans and the software bill of materials (SBOM), and cargo-chef builds the dependencies in a
# layer of their own, which a change to the API's own code leaves cached.
FROM --platform=$BUILDPLATFORM rust:1.99.1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS tools
COPY --from=zig /zig /opt/zig
ENV PATH=/opt/zig:$PATH
WORKDIR /src
COPY rust-toolchain.toml .
RUN rustup toolchain install && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
RUN cargo install --locked cargo-zigbuild@0.24.0 cargo-auditable@0.7.7 cargo-chef@0.1.78

FROM tools AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Static binaries against musl, so the image needs no C library.
FROM tools AS build
ENV SQLX_OFFLINE=true
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --zigbuild --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl --package app --bin antworker-api --recipe-path recipe.json
COPY . .
RUN cargo auditable zigbuild --release --locked --package app --bin antworker-api --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl \
    && mkdir -p /out/amd64 /out/arm64 \
    && cp target/x86_64-unknown-linux-musl/release/antworker-api /out/amd64/ \
    && cp target/aarch64-unknown-linux-musl/release/antworker-api /out/arm64/

# distroless's static base holds the operating system's certificate store (D38), time zone data
# and the unprivileged user 65532, and no shell or package manager.
FROM gcr.io/distroless/static-debian13:nonroot@sha256:18f4cc26c9ffa7f20a359298be7deadac5c607c8e03a02b8119f1a5fc2f82c99
ARG TARGETARCH
COPY --from=build /out/$TARGETARCH/antworker-api /usr/local/bin/antworker-api
USER 65532:65532
# health waits up to 3 seconds for the connection, and up to 3 for each read of the answer
# (PLATFORM-044); a check that runs past its 5-second timeout fails. A one-shot subcommand, such
# as migrate, serves nothing, so its checks fail; the start period keeps a short run from being
# marked unhealthy.
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s --start-interval=1s --retries=3 \
    CMD ["/usr/local/bin/antworker-api", "health"]
ENTRYPOINT ["/usr/local/bin/antworker-api"]
CMD ["serve"]
