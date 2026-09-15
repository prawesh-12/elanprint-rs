#!/bin/sh
set -e
cd "$(dirname "$0")/.."
CAP="$1"
[ -f "$CAP" ] || { echo "usage: decode-capture.sh CAPTURE.pcapng"; exit 1; }
WHERE="${CAP%.pcapng}.where"
if [ -f "$WHERE" ]; then
    read -r BUS DEV < "$WHERE"
else
    . "$(dirname "$0")/usb-locate.sh"
    elanprint_locate || exit 1
    BUS=$ELANPRINT_BUS
    DEV=$ELANPRINT_DEV
    echo "no .where beside the capture, using the sensor's current address" >&2
fi
mkdir -p captures/decoded
OUT="captures/decoded/$(basename "${CAP%.pcapng}").txt"
python3 tools/usbmon_filter.py "$CAP" "$BUS" "$DEV" > "$OUT"
echo "decoded bus $BUS device $DEV -> $OUT"
