#!/bin/sh
# Run the backend daemon and the frontend app together, one command.
# Both use the session bus, so no root and no fight with fprintd over the
# system name. The system bus stays untouched until GATE 6.
# Usage: ./tools/run.sh
set -e
cargo build -j 10 -p elanprintd -p elanprint-login
export ELANPRINT_BUS=session
export ELANPRINT_STORE=/tmp/elanprint-prints.json
./target/debug/elanprintd & DAEMON=$!
trap "kill $DAEMON 2>/dev/null" EXIT INT TERM
./target/debug/elanprint-login
