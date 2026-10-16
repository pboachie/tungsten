# SPDX-License-Identifier: AGPL-3.0-only
# Build with the checksum-verified distribution/container directory as context.
FROM alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
RUN apk add --no-cache --upgrade 'zlib>=1.3.2-r1' \
    && installed="$(apk list --installed zlib | cut -d ' ' -f 1)" \
    && case "$(apk version -t "${installed#zlib-}" 1.3.2-r1)" in \
         '='|'>') ;; \
         *) echo 'zlib is below the required patched version' >&2; exit 1 ;; \
       esac
ARG TARGETARCH
COPY --chmod=755 linux-${TARGETARCH}/tungsten /usr/local/bin/tungsten
COPY LICENSE /usr/share/licenses/tungsten/LICENSE
COPY --chown=65532:65532 workspace /work
LABEL org.opencontainers.image.title="tungsten" \
      org.opencontainers.image.description="SDK generator and MCP compiler" \
      org.opencontainers.image.source="https://github.com/pboachie/tungsten" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
USER 65532:65532
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/tungsten"]
CMD ["--help"]
