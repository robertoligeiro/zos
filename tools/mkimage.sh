#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
exec "${PYTHON:-python}" tools/zos.py image
