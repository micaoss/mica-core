# check=skip=InvalidDefaultArgInFrom
# mica-deploy: the device-side deployment client.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=bin mica-deploy /in/bin/
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
ARG MICA_DEB_SOURCE_COMMIT
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-deploy /stage/usr/bin/mica-deploy; \
    install -D -m 0644 /in/copyright       /stage/usr/share/doc/mica-deploy/copyright; \
    pack.sh --root /stage --control /in/control/mica-deploy.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
