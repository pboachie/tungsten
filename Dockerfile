# SPDX-License-Identifier: AGPL-3.0-only
# Build with the checksum-verified distribution/container directory as context.
FROM alpine:3.22@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8
RUN apk add --no-cache --upgrade zlib=1.3.2-r1
ARG TARGETARCH
COPY --chmod=755 linux-${TARGETARCH}/tungsten /usr/local/bin/tungsten
COPY LICENSE /usr/share/licenses/tungsten/LICENSE
COPY --chown=65532:65532 workspace /work
LABEL org.opencontainers.image.source="https://github.com/pboachie/tungsten" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
USER 65532:65532
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/tungsten"]
CMD ["--help"]
