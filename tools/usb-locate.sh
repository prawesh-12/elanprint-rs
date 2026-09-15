#!/bin/sh
# Sets ELANPRINT_BUS, ELANPRINT_DEV, ELANPRINT_USBMON from sysfs.
# Bus and device change on every replug, so nothing may hardcode them.
ELANPRINT_VID=04f3
ELANPRINT_PID=0c90

elanprint_locate() {
    for d in /sys/bus/usb/devices/*; do
        [ -r "$d/idVendor" ] || continue
        [ "$(cat "$d/idVendor")" = "$ELANPRINT_VID" ] || continue
        [ "$(cat "$d/idProduct")" = "$ELANPRINT_PID" ] || continue
        ELANPRINT_BUS=$(cat "$d/busnum")
        ELANPRINT_DEV=$(cat "$d/devnum")
        ELANPRINT_USBMON="usbmon$ELANPRINT_BUS"
        export ELANPRINT_BUS ELANPRINT_DEV ELANPRINT_USBMON
        return 0
    done
    echo "no $ELANPRINT_VID:$ELANPRINT_PID sensor found" >&2
    return 1
}
