# check=skip=InvalidDefaultArgInFrom
# mica-lifecycle: the static mica-runkit, read out of the archive by the
# assembly's kernel component and never installed into a root.
ARG MICA_BUILD_BASE
FROM ${MICA_BUILD_BASE} AS pack
COPY --from=tools package.json /tools/package.json
COPY --from=tools src /tools/src
COPY --from=pkgs copyright /in/copyright
COPY control/ /in/control/
COPY --from=bin mica-runkit /in/bin/
ARG MICA_DEB_ARCH
ARG SOURCE_DATE_EPOCH
ARG MICA_DEB_SOURCE_REPO
RUN set -eu; \
    install -D -m 0755 /in/bin/mica-runkit /stage/usr/lib/mica/lifecycle/mica-runkit; \
    install -D -m 0644 /in/copyright       /stage/usr/share/doc/mica-lifecycle/copyright; \
    bun /tools/src/cli.ts deb pack --root /stage --control /in/control/mica-lifecycle.control \
            --arch "${MICA_DEB_ARCH}" --out /out
FROM scratch AS out
COPY --from=pack /out/ /
