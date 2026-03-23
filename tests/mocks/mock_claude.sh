#!/bin/bash
# Mock claude CLI that speaks the stream-json protocol.
# Reads a user message from stdin, emits stream events on stdout.

MODEL=""
SYSTEM=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --model) MODEL="$2"; shift 2 ;;
        --system-prompt) SYSTEM="$2"; shift 2 ;;
        *) shift ;;
    esac
done

# Read one line of JSON from stdin (the user message)
read -r INPUT_LINE

# Extract the user prompt text from the JSON
PROMPT=$(echo "$INPUT_LINE" | sed 's/.*"content":"\([^"]*\)".*/\1/')

# Build the response text
RESPONSE="mock response"
if [ -n "$MODEL" ]; then
    RESPONSE="$RESPONSE model=$MODEL"
fi
if [ -n "$SYSTEM" ]; then
    RESPONSE="$RESPONSE system=$SYSTEM"
fi

# Emit stream events (content_block_delta then result)
echo "{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"$RESPONSE\"}}}"
echo "{\"type\":\"result\",\"result\":\"$RESPONSE\",\"is_error\":false,\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}"
