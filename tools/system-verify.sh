#!/bin/sh
# One verify against the installed system daemon. Read only.
#
# Usage: system-verify.sh FINGER TAG
# Starts a signal monitor and a capture, claims, then starts the verify and
# returns. Read /tmp/sysverify-TAG.log only after the user confirms the
# touch, per D-011.
set -e
cd "$(dirname "$0")/.."
FINGER="$1"
TAG="$2"
[ -n "$FINGER" ] && [ -n "$TAG" ] || { echo "usage: system-verify.sh FINGER TAG"; exit 1; }
mkdir -p captures

D=/net/reactivated/Fprint/Device/0
I=net.reactivated.Fprint.Device

dumpcap -i usbmon1 -s 160 -w "captures/sysverify-$TAG.pcapng" -q >/dev/null 2>&1 &
echo "$!" > "/tmp/sysverify-$TAG.cap.pid"
sleep 2

# gdbus uses AddMatch, which a normal user may do. busctl monitor needs
# BecomeMonitor, which is root only on the system bus.
gdbus monitor --system --dest net.reactivated.Fprint \
  > "/tmp/sysverify-$TAG.log" 2>&1 &
echo "$!" > "/tmp/sysverify-$TAG.mon.pid"
sleep 1

busctl --system call net.reactivated.Fprint "$D" "$I" Claim s "$(id -un)" >/dev/null
busctl --system call net.reactivated.Fprint "$D" "$I" VerifyStart s "$FINGER" >/dev/null
echo "verify started for $FINGER. touch now."
