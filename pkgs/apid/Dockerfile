# check=skip=InvalidDefaultArgInFrom
# mica-apid: the API daemon with its embedded UI, its unit (enabled by a link in
# the payload) and the OpenAPI document.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=apid-dist apid.service /in/dist/
COPY --from=bin mica-apid /in/bin/
COPY --from=bin openapi.json /in/openapi.json
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-apid     /stage/usr/bin/mica-apid; \
    install -D -m 0644 /in/dist/apid.service /stage/usr/lib/systemd/system/apid.service; \
    install -D -m 0644 /in/copyright         /stage/usr/share/doc/mica-apid/copyright; \
    install -D -m 0644 /in/openapi.json      /stage/usr/share/mica-apid/openapi.json; \
    install -d -m 0755 /stage/etc/systemd/system/multi-user.target.wants; \
    ln -s /usr/lib/systemd/system/apid.service /stage/etc/systemd/system/multi-user.target.wants/apid.service; \
    pack.sh --root /stage --control /in/control/mica-apid.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
