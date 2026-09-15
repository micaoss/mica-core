# check=skip=InvalidDefaultArgInFrom
# mica-sftp-server: the SFTP server dropbear runs as /usr/lib/sftp-server.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=bin mica-sftp-server /in/bin/
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
ARG MICA_DEB_SOURCE_COMMIT
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-sftp-server /stage/usr/lib/sftp-server; \
    install -D -m 0644 /in/copyright            /stage/usr/share/doc/mica-sftp-server/copyright; \
    pack.sh --root /stage --control /in/control/mica-sftp-server.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
