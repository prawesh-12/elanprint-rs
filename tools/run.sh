#!/bin/sh
# Run the backend daemon and the frontend app together, one command.
# Both use the session bus, so no root and no fight with fprintd over the
# system name. The system bus stays untouched until GATE 6.
# Usage: ./tools/run.sh
set -e
cargo build -j 10 -p elanmocd -p elanmoc-login
export ELANMOC_BUS=session
export ELANMOC_STORE=/tmp/elanmoc-prints.json
./target/debug/elanmocd & DAEMON=$!
trap "kill $DAEMON 2>/dev/null" EXIT INT TERM
./target/debug/elanmoc-login
