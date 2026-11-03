#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md (without its heading), for release notes.
# Prints nothing when the changelog has no section for that version.
# Usage: changelog-notes.sh <version>
set -euo pipefail
awk -v heading="## [$1]" '
    /^## \[/ { if (found) exit; found = (index($0, heading) == 1); next }
    /^\[[^]]+\]: / { if (found) exit }
    found
' "$(dirname "$0")/../../CHANGELOG.md" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}'
