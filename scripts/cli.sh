#!/bin/bash
START=$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000')
bun packages/tokenx/src/index.ts "$@"
EXIT=$?
END=$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000')
echo -e "\n⏱  Done in $((END - START))ms"
exit $EXIT
