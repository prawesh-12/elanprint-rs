#!/bin/sh
# Undo tools/install.sh. Password login is never affected.
#
# The store is KEPT by default. Templates live in the sensor's flash and are
# not touched here. Deleting the store while they remain orphans them: they
# still authenticate and nothing on the host records that they exist.
#
#   --delete-store   remove the name to slot mapping as well
#   --wipe-device    erase every template from the sensor, with confirmation
set -e
cd "$(dirname "$0")/.."

[ "$(id -u)" = "0" ] || { echo "run with sudo"; exit 1; }

DELETE_STORE=0
WIPE_DEVICE=0
for arg in "$@"; do
    case "$arg" in
        --delete-store) DELETE_STORE=1 ;;
        --wipe-device)  WIPE_DEVICE=1 ;;
        *) echo "unknown option: $arg"; exit 1 ;;
    esac
done

STORE=/var/lib/elanprint/prints.json

# installed copy first, so this works without the build tree
find_cli() {
    for c in /usr/libexec/elanprint-cli target/release/elanprint-cli target/debug/elanprint-cli; do
        [ -x "$c" ] && { echo "$c"; return 0; }
    done
    return 1
}

count_templates() {
    CLI=$(find_cli) || return 1
    "$CLI" info 2>/dev/null | sed -n 's/^enrolled: *//p' | tr -d ' '
}

echo "removing"

if systemctl list-unit-files elanprintd.service >/dev/null 2>&1; then
    echo "  stopping elanprintd"
    systemctl disable --now elanprintd.service 2>/dev/null || true
fi
rm -f /etc/systemd/system/elanprintd.service
systemctl daemon-reload
echo "  unit removed"

if systemctl list-unit-files fprintd.service >/dev/null 2>&1; then
    systemctl unmask fprintd.service 2>/dev/null || true
    echo "  fprintd.service unmasked"
fi

if [ "$WIPE_DEVICE" = "1" ]; then
    echo
    echo "erasing every template from the sensor"
    CLI=$(find_cli) || { echo "no elanprint-cli found, nothing erased"; exit 1; }
    "$CLI" wipe-device
    echo
fi

# the cli goes last, --wipe-device above needs it
rm -f /usr/libexec/elanprintd
echo "  binary removed"

rm -f /usr/bin/elanprint-rs
rm -f /usr/share/applications/elanprint-rs.desktop
for s in 48 64 128 256 512; do
    rm -f "/usr/share/icons/hicolor/${s}x${s}/apps/elanprint-rs.png"
done
command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && gtk-update-icon-cache -qf /usr/share/icons/hicolor 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 \
    && update-desktop-database -q /usr/share/applications 2>/dev/null || true
echo "  app, desktop entry and icons removed"

if [ -f /etc/udev/rules.d/70-elanprint.rules ]; then
    rm -f /etc/udev/rules.d/70-elanprint.rules
    udevadm control --reload-rules
    echo "  udev rule removed"
fi

REMAINING=$(count_templates || echo "")
rm -f /usr/libexec/elanprint-cli

if [ "$DELETE_STORE" = "1" ]; then
    rm -rf /var/lib/elanprint
    echo "  store removed"
else
    [ -f "$STORE" ] && echo "  store kept at $STORE" || echo "  no store to keep"
fi

echo
if [ "$WIPE_DEVICE" = "1" ]; then
    echo "The sensor was wiped. Nothing is left that can authenticate."
    exit 0
fi

case "$REMAINING" in
    ""|0)
        [ "$REMAINING" = "0" ] \
            && echo "The sensor holds no templates." \
            || echo "Could not read the sensor, so the template count is unknown."
        ;;
    *)
        echo "The sensor still holds $REMAINING template(s) in its own flash."
        echo
        echo "They were not removed and cannot be listed: the chip answers the"
        echo "same bytes for an occupied slot and an empty one. They will still"
        echo "authenticate after a reinstall, including for a different account,"
        echo "because the login path matches against every template on the chip."
        echo
        echo "If this machine is changing hands, erase them:"
        echo "    sudo ./tools/uninstall.sh --wipe-device"
        ;;
esac

if [ "$DELETE_STORE" = "1" ] && [ -n "$REMAINING" ] && [ "$REMAINING" != "0" ]; then
    echo
    echo "WARNING: the store was deleted while $REMAINING template(s) remain."
    echo "Nothing on this host now records that they exist."
fi
