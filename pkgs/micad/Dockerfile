# check=skip=InvalidDefaultArgInFrom
# micad: the daemon, its unit (enabled by a link in the payload) and its D-Bus policy.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=micad-dist micad.service com.mica.micad.conf /in/dist/
COPY --from=bin micad /in/bin/
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/micad                /stage/usr/bin/micad; \
    install -D -m 0644 /in/dist/micad.service       /stage/usr/lib/systemd/system/micad.service; \
    install -D -m 0644 /in/dist/com.mica.micad.conf /stage/usr/share/dbus-1/system.d/com.mica.micad.conf; \
    install -D -m 0644 /in/copyright                /stage/usr/share/doc/micad/copyright; \
    install -d -m 0755 /stage/etc/systemd/system/multi-user.target.wants; \
    ln -s /usr/lib/systemd/system/micad.service /stage/etc/systemd/system/multi-user.target.wants/micad.service; \
    pack.sh --root /stage --control /in/control/micad.control \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
