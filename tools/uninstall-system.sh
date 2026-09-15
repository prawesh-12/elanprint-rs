#!/bin/sh
# Undo tools/install-system.sh completely. Run with sudo.
#
# Afterwards the stock fprintd is unmasked and the machine is as it was.
# Password login at the greeter is never affected either way.
set -e

[ "$(id -u)" = "0" ] || { echo "run with sudo"; exit 1; }

systemctl disable --now elanprintd.service 2>/dev/null || true
rm -f /etc/systemd/system/elanprintd.service
systemctl daemon-reload
systemctl unmask fprintd.service
rm -f /usr/libexec/elanprintd
echo "removed."
echo "/var/lib/elanprint/prints.json left in place, delete it by hand if you want it gone."
