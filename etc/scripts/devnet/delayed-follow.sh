#!/bin/sh

set -eu

delay="${L2_FOLLOW_START_DELAY_SECS:-0}"

while [ "$delay" -gt 0 ]; do
    mins=$((delay / 60))
    secs=$((delay % 60))
    printf 'Delaying startup to demonstrate catch-up... %02d:%02d remaining\n' "$mins" "$secs"
    sleep 1
    delay=$((delay - 1))
done

exec /app/base-consensus "$@"
