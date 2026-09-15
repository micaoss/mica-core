#!/usr/bin/env bash
# scripts/build/release.sh against a fixture: a git repository holding this
# tree's producers, synthetic archives packed with dpkg-deb, a fake `gh` that
# serves one release and records uploads, a fake OCI registry on 127.0.0.1 with
# a bearer-token challenge, and file:// downloads. It runs inside
# the mica-build-env base image for dpkg-deb and python3; no network and no GitHub.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
command -v docker >/dev/null 2>&1 || { echo "error: docker is required and not on PATH" >&2; exit 1; }
IMAGE="$(bash "${REPO_ROOT}/scripts/build/from.sh" --ref base)"

docker run --rm --label ai-agent=true \
    -v "${REPO_ROOT}:/src:ro" \
    --entrypoint /bin/bash "${IMAGE}" -c '
set -euo pipefail
PASS=0
FAIL=0
T=/work
mkdir -p "${T}/bin"
export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid

# A fake gh: one release per test in ${FAKE}, uploads land in ${FAKE}/download/<tag>/.
cat >"${T}/bin/gh" <<"GH"
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
"api repos/"*)
    [ -f "${FAKE}/release.json" ] || { echo "gh: HTTP 404: Not Found" >&2; exit 1; }
    cat "${FAKE}/release.json" ;;
"release upload")
    tag="$3"; shift 3
    mkdir -p "${FAKE}/download/${tag}"
    while [ "$#" -gt 0 ]; do
        case "$1" in -R) shift 2; continue ;; esac
        n="$(basename "$1")"
        cp "$1" "${FAKE}/download/${tag}/${n}"
        d="sha256:$(sha256sum "$1" | cut -d" " -f1)"
        jq --arg n "${n}" --arg d "${d}" ".assets += [{name: \$n, state: \"uploaded\", digest: \$d}]" "${FAKE}/release.json" >"${FAKE}/r.tmp"
        mv "${FAKE}/r.tmp" "${FAKE}/release.json"
        echo "${n}" >>"${FAKE}/uploads"
        shift
    done ;;
*) echo "gh: unexpected $*" >&2; exit 2 ;;
esac
GH
chmod +x "${T}/bin/gh"
export PATH="${T}/bin:${PATH}"

# A fake registry: the token endpoint answers "push" to basic credentials and
# "anon" to none; ${root}/private refuses anonymous reads and
# ${root}/refuse-push refuses credentials. Blobs and manifests land in ${root}.
cat >"${T}/registry.py" <<"PY"
import hashlib, http.server, json, os, sys, urllib.parse, uuid
ROOT_FILE = sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def send(self, code, body=b"", headers=None):
        self.send_response(code)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
    def route(self):
        root = open(ROOT_FILE).read().strip()
        url = urllib.parse.urlparse(self.path)
        query = urllib.parse.parse_qs(url.query)
        header = self.headers.get("Authorization", "")
        if url.path == "/token":
            push = header.startswith("Basic ")
            if push and os.path.exists(os.path.join(root, "refuse-push")):
                return self.send(401, b"{}")
            return self.send(200, json.dumps({"token": "push" if push else "anon"}).encode())
        if not header.startswith("Bearer "):
            realm = "http://127.0.0.1:%d/token" % self.server.server_address[1]
            return self.send(401, b"{}", {"WWW-Authenticate": "Bearer realm=\"%s\",service=\"fake\"" % realm})
        token = header[len("Bearer "):]
        if url.path == "/v2/":
            return self.send(200, b"{}")
        if self.command in ("POST", "PUT") and token != "push":
            return self.send(403, b"{}")
        if token == "anon" and os.path.exists(os.path.join(root, "private")):
            return self.send(401, b"{}")
        parts = url.path.split("/")
        kind, ref = parts[4], "/".join(parts[5:])
        blobs, manifests = os.path.join(root, "blobs"), os.path.join(root, "manifests")
        os.makedirs(blobs, exist_ok=True)
        os.makedirs(manifests, exist_ok=True)
        body = self.rfile.read(int(self.headers.get("Content-Length", "0"))) if self.command in ("POST", "PUT") else b""
        if kind == "blobs" and ref.startswith("uploads"):
            if self.command == "POST":
                return self.send(202, b"", {"Location": "/v2/%s/%s/blobs/uploads/%s" % (parts[2], parts[3], uuid.uuid4().hex)})
            digest = query["digest"][0]
            if "sha256:" + hashlib.sha256(body).hexdigest() != digest:
                return self.send(400, b"{}")
            open(os.path.join(blobs, digest), "wb").write(body)
            return self.send(201, b"", {"Docker-Content-Digest": digest})
        path = os.path.join(blobs if kind == "blobs" else manifests, ref)
        if kind == "manifests" and self.command == "PUT":
            open(path, "wb").write(body)
            open(os.path.join(root, "manifest-puts"), "a").write(ref + "\n")
            return self.send(201, b"", {"Docker-Content-Digest": "sha256:" + hashlib.sha256(body).hexdigest()})
        if not os.path.exists(path):
            return self.send(404, b"{}")
        return self.send(200, open(path, "rb").read(), {"Content-Type": "application/vnd.oci.image.manifest.v1+json"})
    do_GET = do_HEAD = do_POST = do_PUT = route
http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
REGISTRY_PORT=5055
: >"${T}/registry-root"
python3 "${T}/registry.py" "${REGISTRY_PORT}" "${T}/registry-root" &
for _ in $(seq 50); do curl -s -o /dev/null "http://127.0.0.1:${REGISTRY_PORT}/token" 2>/dev/null && break; sleep 0.1; done

# The fixture repository: this tree'"'"'s producers and release script, and a bare "remote".
G="${T}/repo"
mkdir -p "${G}"
cp -a /src/Makefile /src/VERSION /src/scripts /src/pkgs /src/locks "${G}/"
(cd /src && cp -a --parents crates/*/dist "${G}/")
printf "_out/\n" >"${G}/.gitignore"
git -C "${G}" init -q -b main
git -C "${G}" add -A
git -C "${G}" commit -q -m fixture
C="$(git -C "${G}" rev-parse HEAD)"
git init -q --bare "${T}/remote.git"
git -C "${G}" push -q "${T}/remote.git" main
git -C "${G}" remote add origin https://github.com/micaoss/mica-core.git
git -C "${G}" update-ref refs/remotes/origin/main "${C}"
TAG=20260914-0300
git -C "${T}/remote.git" tag "${TAG}" "${C}"
VERSION="$(tr -d "[:space:]" <"${G}/VERSION")"
WANT="${VERSION}+git${C:0:12}-1"
mapfile -t PKGS < <(bash "${G}/scripts/deb/producers.sh" | awk "{print \$3}" | tr "," "\n")

deb() { # <package> <arch> <commit> [version] -> into the pool
    local p="$1" a="$2" c="$3" v="${4:-${WANT}}" d="${T}/stage/$1-$2"
    rm -rf "${d}"; mkdir -p "${d}/DEBIAN" "${G}/_out/debs/${a}/pool"
    printf "Package: %s\nVersion: %s\nArchitecture: %s\nMaintainer: x <x@example.invalid>\nDescription: fixture\nMica-Source-Repo: mica-core\nMica-Source-Commit: %s\n" "${p}" "${v}" "${a}" "${c}" >"${d}/DEBIAN/control"
    dpkg-deb --root-owner-group -b "${d}" "${G}/_out/debs/${a}/pool/${p}_${WANT}_${a}.deb" >/dev/null
}
pool() { rm -rf "${G}/_out"; for a in amd64 arm64; do for p in "${PKGS[@]}"; do deb "${p}" "${a}" "${C}"; done; done; }
release_json() { # [draft]
    printf "{\"tag_name\":\"%s\",\"html_url\":\"https://github.com/micaoss/mica-core/releases/tag/%s\",\"draft\":%s,\"assets\":[]}\n" "${TAG}" "${TAG}" "${1:-false}" >"${FAKE}/release.json"
}
run() { # <expect 0|1> <case> <needle> [tag]
    local want="$1" name="$2" needle="$3" tag="${4:-${TAG}}" rc=0 out
    out="$(cd "${G}" && GH_TOKEN=fixture MICA_RELEASE_REGISTRY="http://127.0.0.1:${REGISTRY_PORT}" MICA_RELEASE_GIT="${T}/remote.git" MICA_RELEASE_DOWNLOAD="file://${FAKE}/download" bash scripts/build/release.sh "${tag}" 2>&1)" || rc=$?
    if { [ "${want}" = 0 ] && [ "${rc}" = 0 ]; } || { [ "${want}" = 1 ] && [ "${rc}" != 0 ]; }; then
        if [ "${out#*"${needle}"}" != "${out}" ]; then PASS=$((PASS + 1)); echo "PASS: ${name}"; return; fi
    fi
    FAIL=$((FAIL + 1)); echo "FAIL: ${name}: wanted exit ${want} with \"${needle}\", got ${rc}"; printf "%s\n" "${out}" | sed "s/^/    /"
}
fresh() { export FAKE="${T}/fake-$1"; rm -rf "${FAKE}"; mkdir -p "${FAKE}/registry"; echo "${FAKE}/registry" >"${T}/registry-root"; }
check() { # <case> <command>...
    local name="$1"; shift
    if "$@"; then PASS=$((PASS + 1)); echo "PASS: ${name}"; else FAIL=$((FAIL + 1)); echo "FAIL: ${name}"; fi
}

fresh ok; pool; release_json
run 0 "the archives of every declared package pushed as one pool per architecture, the lock attached and read back" "downloaded anonymously"
check "exactly mica-core.lock and SHA256SUMS uploaded, the lock first" [ "$(tr "\n" " " <"${FAKE}/uploads")" = "mica-core.lock SHA256SUMS " ]
L="${FAKE}/download/${TAG}/mica-core.lock"
check "SHA256SUMS lists exactly mica-core.lock" [ "$(sed "s/^[0-9a-f]*  //" "${FAKE}/download/${TAG}/SHA256SUMS")" = mica-core.lock ]
check "the written lock passes the lock checker" [ "$(bash "${G}/scripts/build/check-lock.sh" lock "${L}")" = valid ]
check "the release row names the tag and the commit" [ "$(sed -n 2p "${L}")" = "$(printf "release\tmica-core\t%s\t%s" "${TAG}" "${C}")" ]
for a in amd64 arm64; do
    M="${FAKE}/registry/manifests/pool.${a}.${TAG}"
    check "the ${a} pool holds exactly the ${a} archives, titled with their real names" [ "$(jq -r "[.layers[] | select(.mediaType == \"application/vnd.mica.deb\") | .annotations[\"org.opencontainers.image.title\"]] | sort | join(\" \")" "${M}")" = "$(cd "${G}/_out/debs/${a}/pool" && ls | LC_ALL=C sort | tr "\n" " " | sed "s/ $//")" ]
    check "the ${a} pool carries the source annotations" [ "$(jq -r "[.artifactType, .annotations[\"org.opencontainers.image.version\"], .annotations[\"org.opencontainers.image.revision\"], .annotations[\"mica.source-repo\"], .annotations[\"mica.source-commit\"], .annotations[\"mica.arch\"], (.annotations[\"org.opencontainers.image.created\"] != null)] | map(tostring) | join(\" \")" "${M}")" = "application/vnd.mica.pool ${TAG} ${C} mica-core ${C} ${a} true" ]
    check "the ${a} pool row names that manifest by digest" grep -qx "$(printf "pool\t%s\tghcr.io/micaoss/mica-core:pool.%s.%s@sha256:%s" "${a}" "${a}" "${TAG}" "$(sha256sum "${M}" | cut -d" " -f1)")" "${L}"
done
check "one package row per archive, its sha256 a layer of its pool" bash -c "
    [ \"\$(grep -c \"^package\" \"${L}\")\" = $(( ${#PKGS[@]} * 2 )) ] || exit 1
    grep \"^package\" \"${L}\" | while IFS=\$(printf \"\\t\") read -r _k p a v s; do
        [ \"\${v}\" = \"${WANT}\" ] && [ \"\${s}\" = \"\$(sha256sum \"${G}/_out/debs/\${a}/pool/\${p}_${WANT}_\${a}.deb\" | cut -d\" \" -f1)\" ] &&
            jq -e --arg d \"sha256:\${s}\" \"any(.layers[]; .digest == \\\$d)\" \"${FAKE}/registry/manifests/pool.\${a}.${TAG}\" >/dev/null || exit 1
    done"
run 0 "a rerun finds both assets attached with the same bytes and uploads nothing" "already carries these assets"
run 0 "a rerun finds each pool tag holding the same manifest" "already holds this manifest"
check "each manifest was put once" [ "$(sort "${FAKE}/registry/manifest-puts" | tr "\n" " ")" = "pool.amd64.${TAG} pool.arm64.${TAG} " ]

fresh repointed; pool; release_json; mkdir -p "${FAKE}/registry/manifests"; printf "{}" >"${FAKE}/registry/manifests/pool.arm64.${TAG}"
run 1 "a pool tag already holding another manifest" "never re-pointed"
fresh private; pool; release_json; touch "${FAKE}/registry/private"
run 1 "a package that cannot be read anonymously" "make ghcr.io/micaoss/mica-core public"
fresh nopush; pool; release_json; touch "${FAKE}/registry/refuse-push"
run 1 "a registry refusing the push token" "refused a push token"

fresh badtag; pool; release_json
run 1 "a tag that is not YYYYMMDD-HHMM" "not a UTC time" v0.1.0
run 1 "a tag that is not a real UTC time" "not a UTC time" 20261399-2599
fresh norelease; pool
run 1 "no published release for the tag" "no published release"
fresh draft; pool; release_json true
run 1 "a draft release" "is a draft"
fresh other-bytes; pool; release_json
n=mica-core.lock
jq --arg n "${n}" ".assets += [{name: \$n, state: \"uploaded\", digest: \"sha256:$(printf "0%.0s" {1..64})\"}]" "${FAKE}/release.json" >"${FAKE}/r" && mv "${FAKE}/r" "${FAKE}/release.json"
run 1 "an asset already attached with other bytes" "never replaced"
fresh extra; pool; release_json
jq ".assets += [{name: \"micad_${VERSION}.git${C:0:12}-1_amd64.deb\", state: \"uploaded\", digest: \"sha256:x\"}]" "${FAKE}/release.json" >"${FAKE}/r" && mv "${FAKE}/r" "${FAKE}/release.json"
run 1 "a release carrying anything but the lock and SHA256SUMS" "carries only"
fresh missing; pool; release_json; rm "${G}/_out/debs/arm64/pool/micad_${WANT}_arm64.deb"
run 1 "a missing archive" "archives"
fresh wrong-commit; pool; release_json; deb mica-deploy amd64 "$(printf "a%.0s" {1..40})"
run 1 "an archive built from another commit" "Mica-Source-Commit"
fresh wrong-version; pool; release_json; deb micad arm64 "${C}" "0.0.1+git000000000000-1"
run 1 "an archive of another version" "is Version"
fresh wrong-tag; pool; release_json
o="$(git -C "${G}" commit-tree -m other "${C}^{tree}")"
git -C "${G}" push -q -f "${T}/remote.git" "${o}:refs/tags/${TAG}"
run 1 "the tag names another commit" "not HEAD"
git -C "${G}" push -q -f "${T}/remote.git" "${C}:refs/tags/${TAG}"
fresh dirty; pool; release_json; echo change >>"${G}/VERSION"
run 1 "a dirty checkout" "uncommitted changes"
git -C "${G}" checkout -q VERSION

echo "RESULT: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" = 0 ]
'
