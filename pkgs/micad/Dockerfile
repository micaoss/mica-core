# check=skip=InvalidDefaultArgInFrom
# micad: the one management executable, its unit (enabled by a link in the
# payload) and its D-Bus policy. mica-apid: the same executable under its own
# name (a symlink), its unit (enabled likewise) and the OpenAPI document.
# mica-apid-ui: the web console apid serves when it is installed.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=micad-dist micad.service com.mica.micad.conf /in/dist/
COPY --from=apid-dist apid.service /in/dist/
COPY --from=bin micad openapi.json /in/bin/
COPY --from=bin ui/ /in/ui/
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/micad                /stage/micad/usr/bin/micad; \
    install -D -m 0644 /in/dist/micad.service       /stage/micad/usr/lib/systemd/system/micad.service; \
    install -D -m 0644 /in/dist/com.mica.micad.conf /stage/micad/usr/share/dbus-1/system.d/com.mica.micad.conf; \
    install -D -m 0644 /in/copyright                /stage/micad/usr/share/doc/micad/copyright; \
    install -d -m 0755 /stage/micad/etc/systemd/system/multi-user.target.wants; \
    ln -s /usr/lib/systemd/system/micad.service /stage/micad/etc/systemd/system/multi-user.target.wants/micad.service; \
    install -d -m 0755 /stage/apid/usr/bin; \
    ln -s micad /stage/apid/usr/bin/mica-apid; \
    install -D -m 0644 /in/dist/apid.service        /stage/apid/usr/lib/systemd/system/apid.service; \
    install -D -m 0644 /in/copyright                /stage/apid/usr/share/doc/mica-apid/copyright; \
    install -D -m 0644 /in/bin/openapi.json         /stage/apid/usr/share/mica-apid/openapi.json; \
    install -d -m 0755 /stage/apid/etc/systemd/system/multi-user.target.wants; \
    ln -s /usr/lib/systemd/system/apid.service /stage/apid/etc/systemd/system/multi-user.target.wants/apid.service; \
    install -d -m 0755 /stage/ui/usr/share/mica-apid; \
    cp -R /in/ui /stage/ui/usr/share/mica-apid/ui; \
    find /stage/ui/usr/share/mica-apid/ui -type d -exec chmod 0755 {} +; \
    find /stage/ui/usr/share/mica-apid/ui -type f -exec chmod 0644 {} +; \
    install -D -m 0644 /in/copyright                /stage/ui/usr/share/doc/mica-apid-ui/copyright; \
    pack.sh --root /stage/micad --control /in/control/micad.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out; \
    pack.sh --root /stage/apid --control /in/control/mica-apid.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out; \
    pack.sh --root /stage/ui --control /in/control/mica-apid-ui.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
