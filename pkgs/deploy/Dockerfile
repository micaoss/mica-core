# check=skip=InvalidDefaultArgInFrom
# mica-deploy: the device-side deployment client, and the trial deadline's
# unit and OpenRC script, enabled by the links it ships.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=tools package.json /tools/package.json
COPY --from=tools src /tools/src
COPY --from=deb shlibs.sh /usr/local/bin/shlibs.sh
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=deploy-dist mica-trial-deadline.service mica-trial-deadline.openrc /in/dist/
COPY --from=bin mica-deploy /in/bin/
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-deploy /stage/usr/bin/mica-deploy; \
    install -D -m 0644 /in/copyright       /stage/usr/share/doc/mica-deploy/copyright; \
    install -D -m 0644 /in/dist/mica-trial-deadline.service /stage/usr/lib/systemd/system/mica-trial-deadline.service; \
    install -D -m 0755 /in/dist/mica-trial-deadline.openrc  /stage/etc/init.d/mica-trial-deadline; \
    install -d -m 0755 /stage/etc/systemd/system/multi-user.target.wants /stage/etc/runlevels/sysinit; \
    ln -s /usr/lib/systemd/system/mica-trial-deadline.service /stage/etc/systemd/system/multi-user.target.wants/mica-trial-deadline.service; \
    ln -s /etc/init.d/mica-trial-deadline /stage/etc/runlevels/sysinit/mica-trial-deadline; \
    shlibs_mica_deploy="$(shlibs.sh /stage "${MICA_DEB_ARCH}")"; \
    bun /tools/src/cli.ts deb pack --root /stage --control /in/control/mica-deploy.control \
            --substitute "shlibs:Depends=${shlibs_mica_deploy}" \
            --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
