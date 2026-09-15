#!/bin/sh
set -e
JOBS=10  # leaves 2 cores free for the desktop
cargo build -j $JOBS
cargo test -j $JOBS
cargo clippy --all-targets -- -D warnings
echo "check: build, tests and lints all pass"
