# check=skip=InvalidDefaultArgInFrom
# mica-mqttd and mica-mqtt-broker: not enabled by their packages (micad starts
# them); each postinst creates its service account.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=packer pack.sh /usr/local/bin/pack.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY maintainer/ /in/maintainer/
COPY --from=mqttd-dist mica-mqttd.service /in/dist/
COPY --from=broker-dist mica-mqtt-broker.service /in/dist/
COPY --from=bin mica-mqttd mica-mqtt-broker /in/bin/
ARG MICA_DEB_VERSION
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-mqttd                /stage/mqttd/usr/bin/mica-mqttd; \
    install -D -m 0644 /in/dist/mica-mqttd.service       /stage/mqttd/usr/lib/systemd/system/mica-mqttd.service; \
    install -d -m 0755 /stage/mqttd/usr/lib/mica/mqtt-applications.d; \
    install -D -m 0644 /in/copyright                    /stage/mqttd/usr/share/doc/mica-mqttd/copyright; \
    install -D -m 0755 /in/bin/mica-mqtt-broker          /stage/broker/usr/bin/mica-mqtt-broker; \
    install -D -m 0644 /in/dist/mica-mqtt-broker.service /stage/broker/usr/lib/systemd/system/mica-mqtt-broker.service; \
    install -D -m 0644 /in/copyright                    /stage/broker/usr/share/doc/mica-mqtt-broker/copyright; \
    pack.sh --root /stage/mqttd --control /in/control/mica-mqttd.control \
            --maintainer-scripts /in/maintainer/mica-mqttd \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out; \
    pack.sh --root /stage/broker --control /in/control/mica-mqtt-broker.control \
            --maintainer-scripts /in/maintainer/mica-mqtt-broker \
            --version "${MICA_DEB_VERSION}" --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
