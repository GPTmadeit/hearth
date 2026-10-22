#!/bin/sh
# Catches undefined or unused names across the UI's modules (no build step, so nothing else would).
cd "$(dirname "$0")/../ui" && npx --yes eslint@8.57.0 --no-eslintrc --max-warnings 0 \
  --env browser,es2022 --parser-options=ecmaVersion:2022,sourceType:module \
  --rule 'no-undef: error' --rule 'no-unused-vars: [warn, {args: none}]' \
  --global VideoEncoder,VideoDecoder,VideoFrame,EncodedVideoChunk,MediaStreamTrackProcessor \
  ./*.js
