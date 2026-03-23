#!/bin/bash
# Mock gh CLI that echoes request details as JSON.
# Usage: mock_gh.sh api <path> --method <METHOD> [--input -] [-H <header>]

# Skip "api" arg
shift

PATH_ARG="$1"
shift

METHOD=""
ACCEPT=""
HAS_INPUT=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --method) METHOD="$2"; shift 2 ;;
        --input) HAS_INPUT=true; shift 2 ;;
        -H) ACCEPT="$2"; shift 2 ;;
        *) shift ;;
    esac
done

BODY=""
if $HAS_INPUT; then
    BODY=$(cat)
fi

# Escape body for JSON embedding (handle quotes and backslashes)
ESCAPED_BODY=$(printf '%s' "$BODY" | sed 's/\\/\\\\/g; s/"/\\"/g')

# Return a JSON response echoing the request details
printf '{"path":"%s","method":"%s","accept":"%s","body":"%s"}\n' \
    "$PATH_ARG" "$METHOD" "$ACCEPT" "$ESCAPED_BODY"
