# check=skip=InvalidDefaultArgInFrom
# Two core components, each two pool items (mica-build-tools:docs/spec/release-lock.md
# 1.2.7), composed over
# the root at boot:
#   micad         the one management executable, also apid under its own name
#                 (a symlink), their units (enabled by links in the tree),
#                 their OpenRC scripts and runlevel links, the D-Bus policy and
#                 the OpenAPI document;
#   mica-apid-ui  the web console apid serves when it is present.
# The trees are staged and packed on the build platform: a squashfs and its
# hash tree do not depend on the target architecture, and the binaries were
# built for it by prepare.sh.
ARG MICA_CORE_TOOLS
FROM --platform=$BUILDPLATFORM ${MICA_CORE_TOOLS} AS pack
RUN apk add --no-cache squashfs-tools cryptsetup jq
COPY --from=deb core-image.sh /usr/local/bin/core-image.sh
COPY --from=pkgs copyright /in/copyright
COPY core/ /in/core/
COPY --from=micad-dist micad.service micad.openrc com.mica.micad.conf /in/dist/
COPY --from=apid-dist apid.service apid.openrc /in/dist/
COPY --from=bin micad openapi.json /in/bin/
COPY --from=bin ui/ /in/ui/
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
RUN set -eu; \
    t=/stage/micad; \
    install -D -m 0755 /in/bin/micad                ${t}/usr/bin/micad; \
    ln -s micad                                     ${t}/usr/bin/mica-apid; \
    install -D -m 0644 /in/dist/micad.service       ${t}/usr/lib/systemd/system/micad.service; \
    install -D -m 0644 /in/dist/apid.service        ${t}/usr/lib/systemd/system/apid.service; \
    install -D -m 0755 /in/dist/micad.openrc        ${t}/etc/init.d/micad; \
    install -D -m 0755 /in/dist/apid.openrc         ${t}/etc/init.d/apid; \
    install -D -m 0644 /in/dist/com.mica.micad.conf ${t}/usr/share/dbus-1/system.d/com.mica.micad.conf; \
    install -D -m 0644 /in/bin/openapi.json         ${t}/usr/share/mica-apid/openapi.json; \
    install -D -m 0644 /in/copyright                ${t}/usr/share/doc/micad/copyright; \
    install -d -m 0755 ${t}/etc/systemd/system/multi-user.target.wants ${t}/etc/runlevels/default; \
    for unit in micad apid; do \
        ln -s /usr/lib/systemd/system/${unit}.service ${t}/etc/systemd/system/multi-user.target.wants/${unit}.service; \
        ln -s /etc/init.d/${unit} ${t}/etc/runlevels/default/${unit}; \
    done; \
    u=/stage/ui; \
    install -d -m 0755 ${u}/usr/share/mica-apid; \
    cp -R /in/ui ${u}/usr/share/mica-apid/ui; \
    find ${u}/usr/share/mica-apid/ui -type d -exec chmod 0755 {} +; \
    find ${u}/usr/share/mica-apid/ui -type f -exec chmod 0644 {} +; \
    install -D -m 0644 /in/copyright                ${u}/usr/share/doc/mica-apid-ui/copyright; \
    core-image.sh /stage/micad /in/core/micad.json "${MICA_DEB_ARCH}" "${SOURCE_DATE_EPOCH}" /out; \
    core-image.sh ${u} /in/core/mica-apid-ui.json "${MICA_DEB_ARCH}" "${SOURCE_DATE_EPOCH}" /out
FROM scratch AS out
COPY --from=pack /out/ /
