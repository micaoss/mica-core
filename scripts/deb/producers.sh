#!/usr/bin/env bash
# List and validate the package producers under pkgs/.
#
#   bash scripts/deb/producers.sh                   one line per producer:
#                                                   <name> <dir> <packages,> <enablement,>
#   bash scripts/deb/producers.sh --dir-for <name>  the producer's directory
#
# A producer is pkgs/<name>/ with producer.env, Dockerfile, prepare.sh and a
# control/<package>.control per package. producer.env holds plain assignments:
#
#   PACKAGES="<package> ..."        the Debian packages it emits
#   ENABLEMENT="<package>=<n> ..."  multi-user.target.wants links each ships
#   CONTEXTS="<name>=<path> ..."    extra build contexts (repository-relative)
#   VERSION="<upstream>-<revision>" the version of every package it emits
#   SOURCE_DATE_EPOCH="<seconds>"   every mtime in its packages, bumped with VERSION
#
# A dependency on micad in a control template names micad's declared version.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { echo "producers.sh: error: $*" >&2; exit 1; }

DIR_FOR=""
case "${1:-}" in
"") ;;
--dir-for) DIR_FOR="${2:?--dir-for takes a producer name}" ;;
*) die "usage: bash scripts/deb/producers.sh [--dir-for <name>]" ;;
esac

ROWS=()
declare -A OWNER=() VERSIONS=()
for env_file in "${REPO_ROOT}"/pkgs/*/producer.env; do
    [ -f "${env_file}" ] || continue
    dir="$(dirname "${env_file}")"
    rel="${dir#"${REPO_ROOT}"/}"
    name="$(basename "${dir}")"
    for f in Dockerfile prepare.sh; do
        [ -f "${dir}/${f}" ] || die "${rel} has no ${f}"
    done
    while IFS= read -r line; do
        case "${line}" in
        '' | '#'*) ;;
        PACKAGES=* | ENABLEMENT=* | CONTEXTS=* | VERSION=* | SOURCE_DATE_EPOCH=*)
            case "${line}" in *'$'* | *'`'*) die "${rel}/producer.env: no expansions allowed: ${line}" ;; esac
            ;;
        *) die "${rel}/producer.env: not PACKAGES, ENABLEMENT, CONTEXTS, VERSION or SOURCE_DATE_EPOCH: ${line}" ;;
        esac
    done <"${env_file}"
    PACKAGES="" ENABLEMENT="" CONTEXTS="" VERSION="" SOURCE_DATE_EPOCH=""
    # shellcheck disable=SC1090
    . "${env_file}"
    [ -n "${PACKAGES// /}" ] || die "${rel}/producer.env declares no PACKAGES"
    [[ "${VERSION}" =~ ^[0-9][A-Za-z0-9.+~]*-[A-Za-z0-9.+~]+$ ]] || die "${rel}/producer.env: VERSION '${VERSION}' is not <upstream>-<revision>"
    [[ "${SOURCE_DATE_EPOCH}" =~ ^[1-9][0-9]*$ ]] || die "${rel}/producer.env: SOURCE_DATE_EPOCH '${SOURCE_DATE_EPOCH}' is not a whole number of seconds"
    VERSIONS["${name}"]="${VERSION}"

    templates="$(find "${dir}/control" -maxdepth 1 -name '*.control' -printf '%f\n' 2>/dev/null | sed 's/\.control$//' | LC_ALL=C sort | tr '\n' ' ')"
    declared="$(printf '%s\n' ${PACKAGES} | LC_ALL=C sort | tr '\n' ' ')"
    [ "${templates}" = "${declared}" ] || die "${rel}: PACKAGES is '${declared% }' but control/ holds '${templates% }'"

    for pkg in ${PACKAGES}; do
        [ -z "${OWNER[${pkg}]:-}" ] || die "${pkg} is declared by ${OWNER[${pkg}]} and ${name}"
        OWNER["${pkg}"]="${name}"
        count="$(printf '%s\n' ${ENABLEMENT} | sed -n "s/^${pkg}=//p")"
        [[ "${count}" =~ ^[0-9]+$ ]] || die "${rel}/producer.env: ENABLEMENT has no <count> for ${pkg}"
    done
    for e in ${ENABLEMENT}; do
        case " ${PACKAGES} " in *" ${e%%=*} "*) ;; *) die "${rel}/producer.env: ENABLEMENT names ${e%%=*}, which it does not emit" ;; esac
    done
    for c in ${CONTEXTS}; do
        [[ "${c}" =~ ^[a-z][a-z0-9-]*=.+$ ]] || die "${rel}/producer.env: CONTEXTS entry '${c}' is not <name>=<path>"
        [ -d "${REPO_ROOT}/${c#*=}" ] || die "${rel}/producer.env: context ${c} is not a directory"
    done

    [ "${DIR_FOR}" != "${name}" ] || { echo "${rel}"; exit 0; }
    ROWS+=("${name} ${rel} $(printf '%s' "${PACKAGES}" | tr -s ' ' ',') $(printf '%s' "${ENABLEMENT}" | tr -s ' ' ',')")
done
[ -z "${DIR_FOR}" ] || die "'${DIR_FOR}' is not a producer under pkgs/"
# A package built here that depends on micad names micad's exact declared version.
[ -n "${VERSIONS[micad]:-}" ] || die "no producer micad under pkgs/"
while IFS= read -r template; do
    while IFS= read -r pin; do
        [ "${pin}" = "${VERSIONS[micad]}" ] || die "${template#"${REPO_ROOT}"/} depends on micad (= ${pin}), but pkgs/micad/producer.env declares ${VERSIONS[micad]}"
    done < <(sed -n 's/.*\bmicad (= \([^)]*\)).*/\1/p' "${template}")
    if grep -c 'micad (' "${template}" >/dev/null && ! grep -c 'micad (= ' "${template}" >/dev/null; then
        die "${template#"${REPO_ROOT}"/} depends on micad without its exact version"
    fi
done < <(find "${REPO_ROOT}"/pkgs/*/control -name '*.control' | LC_ALL=C sort)
[ "${#ROWS[@]}" -gt 0 ] || die "no producer under pkgs/"
printf '%s\n' "${ROWS[@]}"
