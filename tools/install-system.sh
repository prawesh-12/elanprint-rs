#!/bin/sh
# Install elanmocd as the system fingerprint daemon. Run with sudo.
#
# Touches no file in /etc/pam.d. GDM's greeter already has a dedicated
# fingerprint stack (/etc/pam.d/gdm-fingerprint, auth required pam_fprintd.so)
# that does not include common-auth, so greeter login works without any PAM
# edit. Password login at the greeter is unaffected either way.
#
# Undo with tools/uninstall-system.sh.
set -e
cd "$(dirname "$0")/.."

[ "$(id -u)" = "0" ] || { echo "run with sudo"; exit 1; }
[ -f target/release/elanmocd ] || {
  echo "build first: cargo build --release -p elanmocd"
  exit 1
}

echo "1. binary -> /usr/libexec/elanmocd"
install -D -m 0755 target/release/elanmocd /usr/libexec/elanmocd

echo "2. store -> /var/lib/elanmoc/prints.json"
install -d -m 0700 /var/lib/elanmoc
if [ -f /var/lib/elanmoc/prints.json ]; then
  echo "   keeping the store already there"
else
  SRC="${ELANMOC_SEED:-/tmp/elanmoc-prints.json}"
  [ -f "$SRC" ] || { echo "   no seed store at $SRC"; exit 1; }
  install -m 0600 "$SRC" /var/lib/elanmoc/prints.json
  echo "   seeded from $SRC"
fi
cat /var/lib/elanmoc/prints.json

echo "3. unit -> /etc/systemd/system/elanmocd.service"
install -m 0644 systemd/elanmocd.service /etc/systemd/system/elanmocd.service
systemctl daemon-reload

echo "4. mask fprintd.service"
# The stock fprintd owns net.reactivated.Fprint and has no 0c90 driver, so it
# would answer the greeter with no device. Masked so D-Bus activation cannot
# start it behind this daemon's back. Unmasked again by the uninstall script.
systemctl mask fprintd.service

echo "5. start elanmocd"
# restart, not just enable --now: on a reinstall the unit is already active
# and --now would leave the old binary running.
systemctl enable elanmocd.service
systemctl restart elanmocd.service
sleep 2
systemctl --no-pager --lines=5 status elanmocd.service || true
