#!/bin/sh
# Runs the tests for lib/stats.sh. Exits non-zero if any check fails.
. ./lib/stats.sh

failed=0
check() {
    if [ "$1" = "$2" ]; then
        echo "ok:   $3"
    else
        echo "FAIL: $3 (expected $2, got $1)"
        failed=1
    fi
}

check "$(sum 1 2 3)" 6 "sum of 1 2 3"
check "$(largest 4 9 2)" 9 "largest of 4 9 2"
check "$(average 2 4 6)" 4 "average of 2 4 6"
check "$(average 10)" 10 "average of a single number"
exit $failed
