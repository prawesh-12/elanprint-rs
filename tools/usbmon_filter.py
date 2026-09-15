#!/usr/bin/env python3
"""Extract one USB device's traffic from a usbmon pcapng capture.

Reads pcapng Enhanced Packet Blocks directly and decodes the Linux usbmon
mmapped header (DLT 115, 64 byte header), so tshark is not needed.

Usage: usbmon_filter.py CAPTURE.pcapng [BUS] [DEVICE]
"""

import struct
import sys

EPB = 0x00000006
SHB = 0x0A0D0D0A
IDB = 0x00000001

XFER = {0: "iso", 1: "intr", 2: "ctrl", 3: "bulk"}


def blocks(data):
    off = 0
    endian = "<"
    while off + 12 <= len(data):
        (btype,) = struct.unpack_from(endian + "I", data, off)
        if btype == SHB:
            magic = struct.unpack_from("<I", data, off + 8)[0]
            endian = "<" if magic == 0x1A2B3C4D else ">"
        (blen,) = struct.unpack_from(endian + "I", data, off + 4)
        if blen < 12 or off + blen > len(data):
            break
        yield btype, data[off + 8 : off + blen - 4], endian
        off += blen


def parse(path, bus, dev):
    with open(path, "rb") as fh:
        raw = fh.read()

    out = []
    for btype, body, endian in blocks(raw):
        if btype != EPB:
            continue
        # interface id, ts high, ts low, captured len, original len
        _iface, tsh, tsl, caplen, _origlen = struct.unpack_from(endian + "IIIII", body, 0)
        pkt = body[20 : 20 + caplen]
        if len(pkt) < 64:
            continue

        (
            _urb_id,
            ev_type,
            xfer_type,
            epnum,
            devnum,
            busnum,
            _flag_setup,
            _flag_data,
            _ts_sec,
            _ts_usec,
            status,
            length,
            len_cap,
        ) = struct.unpack_from("<QBBBBHbbqiiII", pkt, 0)

        if busnum != bus or devnum != dev:
            continue

        payload = pkt[64 : 64 + len_cap]
        ts = ((tsh << 32) | tsl) / 1_000_000
        out.append(
            {
                "ts": ts,
                "event": chr(ev_type),
                "xfer": XFER.get(xfer_type, str(xfer_type)),
                "ep": epnum,
                "status": status,
                "length": length,
                "data": payload,
            }
        )
    return out


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    path = sys.argv[1]
    bus = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    dev = int(sys.argv[3]) if len(sys.argv) > 3 else 2

    events = parse(path, bus, dev)
    if not events:
        print(f"no traffic for bus {bus} device {dev}")
        return 0

    base = events[0]["ts"]
    print(f"{len(events)} events for bus {bus} device {dev}")
    print(f"{'t+s':>9}  {'ev':2} {'xfer':4} {'ep':>4} {'st':>4} {'len':>4}  bytes")
    for e in events:
        direction = "IN " if e["ep"] & 0x80 else "OUT"
        hexed = " ".join(f"{b:02x}" for b in e["data"])
        print(
            f"{e['ts'] - base:9.3f}  {e['event']:2} {e['xfer']:4} "
            f"{direction}0x{e['ep'] & 0x7f:02x} {e['status']:>4} {e['length']:>4}  {hexed}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
