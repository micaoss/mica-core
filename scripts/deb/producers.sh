#!/usr/bin/env bash
# List and validate the package producers under pkgs/.
#
#   bash scripts/deb/producers.sh                       one line per producer:
#                                                       <name> <dir> <packages,> <enablement,>
#   bash scripts/deb/producers.sh --dir-for <name>      the producer's directory
#   bash scripts/deb/producers.sh --version-of <name>   the version its packages carry
#   bash scripts/deb/producers.sh --kind-of <package>   deb or core
#
# A producer is pkgs/<name>/ with mica-inputs, producer.env, Dockerfile,
# prepare.sh and, per package, either a control/<package>.control (a Debian
# package) or a core/<package>.json (a core component,
# two pool items, mica-build-tools:docs/spec/release-lock.md 1.2.7).
#
#   mica-inputs    the packages it emits (`package` lines) and what decides
#                  their bytes (mica-build-tools design 3.3.1)
#   producer.env   plain assignments:
#                    ENABLEMENT="<package>=<n> ..."  multi-user.target.wants links each ships
#                    CONTEXTS="<name>=<path> ..."    extra build contexts (repository-relative)
#   control/       one template per Debian package, each declaring the producer's one
#                  Version (<major>.<minor>.<patch>-<revision>) and Source-Date-Epoch
#                  literally (design 3.3.2)
#   core/          one template per core component: package, version
#                  (<major>.<minor>.<patch>),
#                  sourceDateEpoch, features, needs and root (scripts/deb/core-image.sh)
#
# A dependency on a package built here names that package's exact version.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "producers.sh: error: $*" >&2; exit 1; }

DIR_FOR="" VERSION_OF="" KIND_OF=""
case "${1:-}" in
"") ;;
--dir-for) DIR_FOR="${2:?--dir-for takes a producer name}" ;;
--version-of) VERSION_OF="${2:?--version-of takes a producer name}" ;;
--kind-of) KIND_OF="${2:?--kind-of takes a package name}" ;;
*) die "usage: bash scripts/deb/producers.sh [--dir-for <name> | --version-of <name> | --kind-of <package>]" ;;
esac
command -v jq >/dev/null 2>&1 || die "jq is required"

field() { # <template> <field>: the field's value, or empty
    sed -n "s/^$2: *//p" "$1" | awk "NR == 1"
}

ROWS=()
declare -A OWNER=() VERSIONS=() PKG_VERSION=() KIND=()
for inputs in "${REPO_ROOT}"/pkgs/*/mica-inputs; do
    [ -f "${inputs}" ] || continue
    dir="$(dirname "${inputs}")"
    rel="${dir#"${REPO_ROOT}"/}"
    name="$(basename "${dir}")"
    for f in Dockerfile prepare.sh producer.env; do
        [ -f "${dir}/${f}" ] || die "${rel} has no ${f}"
    done
    while IFS= read -r line; do
        case "${line}" in
        '' | '#'*) ;;
        ENABLEMENT=* | CONTEXTS=*)
            case "${line}" in *'$'* | *'`'*) die "${rel}/producer.env: no expansions allowed: ${line}" ;; esac
            ;;
        *) die "${rel}/producer.env: not ENABLEMENT or CONTEXTS: ${line}" ;;
        esac
    done <"${dir}/producer.env"
    ENABLEMENT="" CONTEXTS=""
    # shellcheck disable=SC1091
    . "${dir}/producer.env"
    PACKAGES="$(sed -n 's/^package //p' "${inputs}" | tr '\n' ' ')"
    [ -n "${PACKAGES// /}" ] || die "${rel}/mica-inputs declares no package"

    templates="$( { [ ! -d "${dir}/control" ] || find "${dir}/control" -maxdepth 1 -name '*.control' -printf '%f\n' | sed 's/\.control$//'
        [ ! -d "${dir}/core" ] || find "${dir}/core" -maxdepth 1 -name '*.json' -printf '%f\n' | sed 's/\.json$//'; } | LC_ALL=C sort | tr '\n' ' ')"
    declared="$(printf '%s\n' ${PACKAGES} | LC_ALL=C sort | tr '\n' ' ')"
    [ "${templates}" = "${declared}" ] || die "${rel}: mica-inputs declares '${declared% }' but control/ and core/ hold '${templates% }'"

    version="" epoch=""
    for pkg in ${PACKAGES}; do
        if [ -f "${dir}/core/${pkg}.json" ]; then
            template="${dir}/core/${pkg}.json" KIND["${pkg}"]=core
            [ "$(jq -r '.package' "${template}")" = "${pkg}" ] || die "${rel}/core/${pkg}.json does not name package ${pkg}"
            v="$(jq -r '.version // ""' "${template}")" e="$(jq -r '.sourceDateEpoch // ""' "${template}")"
            [[ "${v}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "${rel}/core/${pkg}.json: version '${v}' is not <major>.<minor>.<patch>"
        else
            template="${dir}/control/${pkg}.control" KIND["${pkg}"]=deb
            v="$(field "${template}" Version)" e="$(field "${template}" Source-Date-Epoch)"
            [[ "${v}" =~ ^[0-9]+\.[0-9]+\.[0-9]+-[1-9][0-9]*$ ]] || die "${rel}/control/${pkg}.control: Version '${v}' is not <major>.<minor>.<patch>-<revision>"
        fi
        t="${template#"${dir}"/}"
        [[ "${e}" =~ ^[1-9][0-9]*$ ]] || die "${rel}/${t}: Source-Date-Epoch '${e}' is not a whole number of seconds"
        [ -z "${version}" ] || [ "${v}" = "${version}" ] || die "${rel}: ${t} declares ${v}, another template ${version}; a producer has one version"
        [ -z "${epoch}" ] || [ "${e}" = "${epoch}" ] || die "${rel}: ${t} declares Source-Date-Epoch ${e}, another template ${epoch}"
        version="${v}" epoch="${e}"
        [ -z "${OWNER[${pkg}]:-}" ] || die "${pkg} is declared by ${OWNER[${pkg}]} and ${name}"
        OWNER["${pkg}"]="${name}"
        PKG_VERSION["${pkg}"]="${v}"
        count="$(printf '%s\n' ${ENABLEMENT} | sed -n "s/^${pkg}=//p")"
        [[ "${count}" =~ ^[0-9]+$ ]] || die "${rel}/producer.env: ENABLEMENT has no <count> for ${pkg}"
    done
    VERSIONS["${name}"]="${version}"
    for e in ${ENABLEMENT}; do
        case " ${PACKAGES} " in *" ${e%%=*} "*) ;; *) die "${rel}/producer.env: ENABLEMENT names ${e%%=*}, which it does not emit" ;; esac
    done
    for c in ${CONTEXTS}; do
        [[ "${c}" =~ ^[a-z][a-z0-9-]*=.+$ ]] || die "${rel}/producer.env: CONTEXTS entry '${c}' is not <name>=<path>"
        [ -d "${REPO_ROOT}/${c#*=}" ] || die "${rel}/producer.env: context ${c} is not a directory"
    done

    [ "${DIR_FOR}" != "${name}" ] || { echo "${rel}"; exit 0; }
    ROWS+=("${name} ${rel} $(printf '%s' "${PACKAGES% }" | tr -s ' ' ',') $(printf '%s' "${ENABLEMENT}" | tr -s ' ' ',')")
done
[ -z "${DIR_FOR}" ] || die "'${DIR_FOR}' is not a producer under pkgs/"
if [ -n "${KIND_OF}" ]; then
    [ -n "${KIND[${KIND_OF}]:-}" ] || die "'${KIND_OF}' is not a package a producer under pkgs/ declares"
    echo "${KIND[${KIND_OF}]}"
    exit 0
fi
if [ -n "${VERSION_OF}" ]; then
    [ -n "${VERSIONS[${VERSION_OF}]:-}" ] || die "'${VERSION_OF}' is not a producer under pkgs/"
    echo "${VERSIONS[${VERSION_OF}]}"
    exit 0
fi
# A package built here that depends on another package built here names its
# exact version.
while IFS= read -r template; do
    for pkg in "${!PKG_VERSION[@]}"; do
        while IFS= read -r pin; do
            [ "${pin}" = "${PKG_VERSION[${pkg}]}" ] ||
                die "${template#"${REPO_ROOT}"/} depends on ${pkg} (= ${pin}), but its template declares ${PKG_VERSION[${pkg}]}"
        done < <(sed -n "/^Depends:/s/.*[ ,]${pkg} (= \([^)]*\)).*/\1/p" "${template}")
        if grep -Ec "^Depends:.*[ ,]${pkg}( \(|,|$)" "${template}" >/dev/null &&
            ! grep -Ec "^Depends:.*[ ,]${pkg} \(= " "${template}" >/dev/null; then
            die "${template#"${REPO_ROOT}"/} depends on ${pkg} without its exact version"
        fi
    done
done < <(find "${REPO_ROOT}"/pkgs/*/control -name '*.control' 2>/dev/null | LC_ALL=C sort)
# The same for a core component's needs: a need of a package built here names
# its exact version, as min and as max.
while IFS= read -r template; do
    while IFS=$'\t' read -r need min max; do
        [ -n "${PKG_VERSION[${need}]:-}" ] || continue
        [ "${min}" = "${PKG_VERSION[${need}]}" ] && [ "${max}" = "${PKG_VERSION[${need}]}" ] ||
            die "${template#"${REPO_ROOT}"/} needs ${need} from ${min} to ${max:-any}, but its template declares ${PKG_VERSION[${need}]}; name its exact version"
    done < <(jq -r '.needs[] | [.package, .min, (.max // "")] | @tsv' "${template}")
done < <(find "${REPO_ROOT}"/pkgs/*/core -name '*.json' 2>/dev/null | LC_ALL=C sort)
[ "${#ROWS[@]}" -gt 0 ] || die "no producer under pkgs/"
printf '%s\n' "${ROWS[@]}"
