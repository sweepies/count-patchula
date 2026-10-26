# The server image, built only by the release workflow (see .github/workflows/release.yml).
# The build context is the workflow's dist/ directory, not the repository: the
# image reuses the smoke-tested musl release archives and compiles nothing.

# Debian 13 slim.
FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad AS base

# mise's own image: a scratch image holding the static binary at /usr/local/bin/mise.
FROM ghcr.io/jdx/mise:2026.10.18@sha256:fc31de7b93d195613deb600d42c65a164b0205716ade38132aadeabac3838248 AS mise

FROM base AS unpack
ARG TARGETARCH
COPY patchula-v*-linux-*-musl.tar.gz /dist/
RUN <<EOF
set -eu
case "$TARGETARCH" in
  amd64) platform=linux-x64-musl ;;
  arm64) platform=linux-arm64-musl ;;
  *) echo "no release archive for $TARGETARCH" >&2; exit 1 ;;
esac
mkdir /out
tar -xzf /dist/patchula-v*-"$platform".tar.gz -C /out ./bin/patchula ./LICENSE
EOF

FROM base
# git is for the partial clones lockfile tools work in, and curl is for the
# health check.
RUN <<EOF
set -eu
apt-get update
apt-get install --yes --no-install-recommends ca-certificates curl git
rm -rf /var/lib/apt/lists/*
groupadd --gid 10001 patchula
useradd --uid 10001 --gid 10001 --create-home --home-dir /home/patchula --shell /usr/sbin/nologin patchula
install --directory --owner patchula --group patchula /var/lib/patchula
EOF
COPY --from=mise /usr/local/bin/mise /usr/local/bin/mise
COPY --from=unpack --chmod=755 /out/bin/patchula /usr/local/bin/patchula
COPY --from=unpack /out/LICENSE /usr/share/doc/patchula/LICENSE
# The settings serve needs. Mount a file over it, or point
# PATCHULA_SERVER_SETTINGS elsewhere, to configure the server.
COPY <<EOF /etc/patchula/server.pkl
amends "PatchulaServer.pkl"
EOF

# The lockfile runner starts each tool with a temporary HOME and a PATH of
# /usr/local/bin:/usr/bin:/bin, and keeps mise's data, downloads and installs
# in PATCHULA_TOOLS_DIR. Everything worth keeping is under the volume,
# including rustup's toolchains, which RUSTUP_HOME would otherwise put under
# HOME.
ENV HOME=/home/patchula \
    PATCHULA_LISTEN=0.0.0.0:8080 \
    PATCHULA_SERVER_SETTINGS=/etc/patchula/server.pkl \
    PATCHULA_SERVER_CACHE=/var/lib/patchula \
    PATCHULA_TOOLS_DIR=/var/lib/patchula/tools \
    RUSTUP_HOME=/var/lib/patchula/rustup

WORKDIR /var/lib/patchula
# After the directory is created and owned: a new volume copies its owner, and
# Docker discards later changes to it.
VOLUME /var/lib/patchula
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD curl --fail --silent --show-error --output /dev/null "http://127.0.0.1:${PATCHULA_LISTEN##*:}/healthz"
ENTRYPOINT ["patchula"]
CMD ["serve"]
