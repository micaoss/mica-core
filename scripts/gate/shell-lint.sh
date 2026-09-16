#!/usr/bin/env bash
# A pipeline whose reader exits early is a lie under `set -o pipefail`.
set -euo pipefail

cd "$(dirname "$0")/../.."

PASS_N=0
FAIL_N=0
pass() { PASS_N=$((PASS_N + 1)); [ -n "${LINT_QUIET:-}" ] || echo "PASS: $1"; }
fail() { FAIL_N=$((FAIL_N + 1)); echo "FAIL: $1"; }

# AN UNRESOLVED MERGE IS REFUSED, not worked around, and the denominator is why.
mapfile -t unmerged < <(git diff --name-only --diff-filter=U | sort -u)
if [ "${#unmerged[@]}" -gt 0 ]; then
    echo "error: this tree has ${#unmerged[@]} unresolved merge conflict(s), so the count below would be wrong in both halves:" >&2
    printf '         %s\n' "${unmerged[@]}" >&2
    echo "       git ls-files lists a conflicted path once per index stage, so each is scanned up to three" >&2
    echo "       times -- and a file still holding conflict markers is not a shell script to scan in the" >&2
    echo "       first place. Resolve the merge and run this again." >&2
    exit 1
fi

# The file list comes from git, so a script added to the tree is covered the day
# it lands. An untracked scratch file is deliberately out of scope. `sort -u`
# and not `sort`: the refusal above is what keeps a conflicted tree out, and this
# is the second half of the same statement -- one entry per path, whatever the
# index holds. The workflows are in the list because a `run:` block that sets
# pipefail is a script like any other, and the scan below reads lines.
mapfile -t files < <(git ls-files '*.sh' 'scripts/build/*' '.github/workflows/*.yml' | sort -u)
[ "${#files[@]}" -gt 0 ] || { echo "error: no shell scripts found; this lint would pass by finding nothing" >&2; exit 1; }

# The readers that stop before their input does. Each one kills the writer on
# its left with SIGPIPE, and under `pipefail` that is the pipeline's status --
# so the same line passes or fails depending on how much the writer had already
# pushed into the pipe buffer before the reader left. One regex, one name and
# one remedy per entry, in the same order.
READERS=(
    '\|[[:space:]]*(command[[:space:]]+)?e?grep([[:space:]]+-[A-Za-z]*q[A-Za-z]*)+'
    '\|[[:space:]]*(command[[:space:]]+)?e?grep([[:space:]]+-[A-Za-z]*m[A-Za-z]*)+[[:space:]]*[0-9]'
    '\|[[:space:]]*head([[:space:]]|$)'
    '\|[[:space:]]*sed[^|]*[0-9]+[[:space:]]*q'
    '\|[[:space:]]*awk[^|]*[[:space:]]exit'
    '\|[[:space:]]*read([[:space:]]|$)'
)
NAMES=(
    'an early-exiting grep'
    'a grep bounded by -m'
    'head'
    'a sed that quits'
    'an awk that exits'
    'a bare read'
)
REMEDIES=(
    "'grep -c ... >/dev/null'"
    "'grep ...' and bound the result afterwards"
    "'awk \"NR <= N\"'"
    "'awk \"NR <= N\"'"
    'an awk that reads to the end of its input'
    "a 'while ... read' loop that runs to the end, or a command substitution"
)

scanned=0
for f in "${files[@]}"; do
    [ -f "${f}" ] || continue
    # The WHOLE file, not its first N lines: `set -euo pipefail` does not have
    # to be near the top, and a scoping heuristic that quietly excludes files
    # is indistinguishable, in the output, from a tree that is clean.
    grep -c 'pipefail' "${f}" >/dev/null || continue
    scanned=$((scanned + 1))
    clean=1
    for i in "${!READERS[@]}"; do
        # A pipe, optional whitespace, then the reader; comment lines dropped
        # afterwards so prose about the trap is not an instance of it.
        hits="$(grep -nE "${READERS[${i}]}" "${f}" | grep -vE '^[0-9]+:[[:space:]]*#' || true)"
        [ -n "${hits}" ] || continue
        clean=0
        while IFS= read -r h; do
            fail "${f}:${h%%:*}: ${NAMES[${i}]} on the right of a pipe, in a file that sets pipefail: the pipeline reports failure exactly when the reader stops early. Use ${REMEDIES[${i}]}"
        done <<<"${hits}"
    done
    [ "${clean}" = 0 ] || pass "${f} pipes nothing into an early-exiting reader"
done

[ "${scanned}" -gt 0 ] || { echo "error: no file enabled pipefail; the scan matched nothing and would report clean" >&2; exit 1; }

echo "RESULT: $([ "${FAIL_N}" -eq 0 ] && echo PASS || echo FAIL) ($((PASS_N))/$((PASS_N + FAIL_N)) files clean, ${scanned} scanned)"
[ "${FAIL_N}" -eq 0 ]
