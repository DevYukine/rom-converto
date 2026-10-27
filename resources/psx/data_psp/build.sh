#!/usr/bin/env bash
# Rebuilds resources/psx/DATA.PSP from this folder in the pinned pspdev image.
set -euo pipefail

readonly SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly IMAGE="pspdev/pspdev:v20261001@sha256:54895e6f5afb71b8f4f6915ee6d5023e1e087ca7afdf2dcb1d7a694a5233e45f"
readonly WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

cp "$SOURCE_DIR"/main.c "$SOURCE_DIR"/popsman.S "$SOURCE_DIR"/Makefile "$WORK_DIR"/
docker run --rm --platform linux/amd64 -v "$WORK_DIR":/src -w /src "$IMAGE" make
cp "$WORK_DIR"/DATA.prx "$SOURCE_DIR"/../DATA.PSP
shasum -a 256 "$SOURCE_DIR"/../DATA.PSP
