#!/bin/sh
# One command for the whole workspace: build, tests, lints.
# Usage: ./tools/check.sh
# Leaves 2 cores free for the desktop, per CLAUDE.md section 6.
set -e
JOBS=10
cargo build -j $JOBS
cargo test -j $JOBS
cargo clippy --all-targets -- -D warnings
echo "check: build, tests and lints all pass"
