#!/bin/sh
# Undo tools/install-system.sh completely. Run with sudo.
#
# Afterwards the stock fprintd is unmasked and the machine is as it was.
# Password login at the greeter is never affected either way.
set -e

[ "$(id -u)" = "0" ] || { echo "run with sudo"; exit 1; }

systemctl disable --now elanmocd.service 2>/dev/null || true
rm -f /etc/systemd/system/elanmocd.service
systemctl daemon-reload
systemctl unmask fprintd.service
rm -f /usr/libexec/elanmocd
echo "removed."
echo "/var/lib/elanmoc/prints.json left in place, delete it by hand if you want it gone."
