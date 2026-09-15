#!/bin/sh
# Read only. Read the log only after the user confirms the touch.
set -e
cd "$(dirname "$0")/.."
FINGER="$1"
TAG="$2"
[ -n "$FINGER" ] && [ -n "$TAG" ] || { echo "usage: system-verify.sh FINGER TAG"; exit 1; }
mkdir -p captures

D=/net/reactivated/Fprint/Device/0
I=net.reactivated.Fprint.Device

# bus and device change on every replug
. "$(dirname "$0")/usb-locate.sh"
elanprint_locate || exit 1
echo "sensor on bus $ELANPRINT_BUS device $ELANPRINT_DEV, capturing $ELANPRINT_USBMON"
echo "$ELANPRINT_BUS $ELANPRINT_DEV" > "captures/sysverify-$TAG.where"

dumpcap -i "$ELANPRINT_USBMON" -s 160 -w "captures/sysverify-$TAG.pcapng" -q >/dev/null 2>&1 &
echo "$!" > "/tmp/sysverify-$TAG.cap.pid"
sleep 2

# gdbus uses AddMatch; busctl monitor needs BecomeMonitor, root only
gdbus monitor --system --dest net.reactivated.Fprint \
  > "/tmp/sysverify-$TAG.log" 2>&1 &
echo "$!" > "/tmp/sysverify-$TAG.mon.pid"
sleep 1

busctl --system call net.reactivated.Fprint "$D" "$I" Claim s "$(id -un)" >/dev/null
busctl --system call net.reactivated.Fprint "$D" "$I" VerifyStart s "$FINGER" >/dev/null
echo "verify started for $FINGER. touch now."
