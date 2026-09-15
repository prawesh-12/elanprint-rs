#!/bin/sh
# Development mode: session bus, throwaway store, nothing installed.
# The real store at /var/lib/elanprint is never read or written.
set -e
cd "$(dirname "$0")/.."

DEV_STORE=/tmp/elanprint-dev.json
export ELANPRINT_BUS=session
export ELANPRINT_STORE=$DEV_STORE

# Dev mode is session bus and a user-owned store. Under sudo the session bus
# address belongs to the wrong user and the store lands root-owned.
if [ "$(id -u)" = "0" ]; then
    echo "run tools/dev.sh as yourself, not with sudo." >&2
    echo "Dev mode uses your session bus and needs no root." >&2
    exit 1
fi

usage() {
    cat <<'EOF'
Usage: tools/dev.sh <command>

  up        daemon and app together, session bus
  daemon    daemon only, foreground, debug logging
  status    sensor, service, store, bus owner
  cli ...   elanprint-cli against the dev setup
  clean     delete the dev store

Dev mode uses the session bus and /tmp/elanprint-dev.json, so the installed
daemon and the real store are untouched. The sensor is the one shared thing:
stop the system service before using it here.
EOF
}

# One process owns USB interface 0. The installed daemon holds it for its
# lifetime, so dev mode cannot open the sensor while it runs.
device_is_free() {
    systemctl is-active --quiet elanprintd.service 2>/dev/null && return 1
    return 0
}

require_free_device() {
    device_is_free && return 0
    echo "elanprintd.service is running and holds the sensor." >&2
    echo "Stop it first:  sudo systemctl stop elanprintd" >&2
    echo "Start it after: sudo systemctl start elanprintd" >&2
    exit 1
}

case "${1:-}" in
up)
    require_free_device
    cargo build -j 6 -p elanprintd -p elanprint-login
    [ -f "$DEV_STORE" ] || echo '{}' > "$DEV_STORE"
    RUST_LOG=${RUST_LOG:-info} ./target/debug/elanprintd &
    DAEMON=$!
    trap 'kill $DAEMON 2>/dev/null' EXIT INT TERM
    sleep 1
    ./target/debug/elanprint-login
    ;;
daemon)
    require_free_device
    cargo build -j 6 -p elanprintd
    [ -f "$DEV_STORE" ] || echo '{}' > "$DEV_STORE"
    echo "session bus, store $DEV_STORE, ctrl-c to stop"
    RUST_LOG=${RUST_LOG:-debug} exec ./target/debug/elanprintd
    ;;
cli)
    shift
    cargo build -j 6 -p elanprint-cli
    exec ./target/debug/elanprint-cli "$@"
    ;;
status)
    . "$(dirname "$0")/usb-locate.sh"
    if elanprint_locate >/dev/null 2>&1; then
        echo "sensor:    04f3:0c90 on bus $ELANPRINT_BUS device $ELANPRINT_DEV"
    else
        echo "sensor:    not found"
    fi
    if systemctl is-active --quiet elanprintd.service 2>/dev/null; then
        echo "service:   active, pid $(systemctl show elanprintd.service -p MainPID --value)"
        echo "           holds the sensor, dev mode cannot open it"
    else
        echo "service:   inactive, sensor is free"
    fi
    OWNER=$(busctl --system status net.reactivated.Fprint 2>/dev/null | sed -n 's/^PID=//p')
    if [ -n "$OWNER" ]; then
        echo "system bus: owned by pid $OWNER"
    else
        echo "system bus: unowned"
    fi
    if [ -f "$DEV_STORE" ]; then
        echo "dev store: $DEV_STORE $(tr -d '\n ' < "$DEV_STORE")"
    else
        echo "dev store: none yet"
    fi
    ;;
clean)
    rm -f "$DEV_STORE"
    echo "removed $DEV_STORE"
    ;;
*)
    usage
    exit 1
    ;;
esac
