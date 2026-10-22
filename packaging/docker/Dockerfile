# The image published as ghcr.io/twiebe/capycal: the static musl binaries of
# the release on distroless (CA certificates, time zones, the nonroot user).
# The context holds <arch>/capycal and the notices, as the release job
# stages them in dist/image:
#   docker buildx build --platform linux/amd64,linux/arm64 -f packaging/docker/Dockerfile dist/image

# distroless has no shell, so the state directory is made here
FROM --platform=$BUILDPLATFORM busybox:1.37 AS dirs
RUN mkdir -p /out/var/lib/capycal /out/etc/capycal

FROM gcr.io/distroless/static-debian13:nonroot
ARG TARGETARCH
# A new volume takes the owner of the directory it is mounted on
COPY --from=dirs --chown=65532:65532 /out/ /
COPY --chmod=755 ${TARGETARCH}/capycal /usr/local/bin/capycal
COPY LICENSE CONTRIBUTORS.md THIRD-PARTY-NOTICES.txt /usr/share/doc/capycal/

# The default config is /etc/capycal/capycal.toml, so options like
# --validate need no --config. The state goes to the volume of compose.yml;
# state_dir in the config wins
ENV XDG_CONFIG_HOME=/etc \
    CAPYCAL_STATE_DIR=/var/lib/capycal

ENTRYPOINT ["/usr/local/bin/capycal"]
