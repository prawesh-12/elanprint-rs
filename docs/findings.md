# findings

Append-only record of observed device behaviour and every difference from the
documented source. The device is the truth. When a response disagrees with
`docs/protocol.md`, the difference gets written here and the parser is not bent
to fit.

Format: one dated entry per observation. Raw bytes as hex, lowercase, space
separated.

---

## 2026-09-14 Task 0.1 descriptors

Source: `lsusb -v -d 04f3:0c90`, saved to `docs/descriptors.txt`.

Captured without root. `sudo` on this machine needs a password I cannot supply,
so lsusb printed "Couldn't open device, some information will be missing" to
stderr. What that omits is live string descriptor reads and the device status
word. Every configuration, interface and endpoint descriptor below came through
complete from the kernel's cached copy, so task 0.1 is fully answered.

Device:

- `bcdUSB 2.00`, but the port negotiates 12 Mbit/s. `lsusb -t` reports
  `Port 005: Dev 002, If 0, Class=Vendor Specific Class, Driver=[none], 12M`.
  Full speed, as plan.md says.
- `bcdDevice 1.08`. `bMaxPacketSize0 8`. `iSerial 0`, no serial number.
- One configuration, one interface, `bInterfaceClass 255`, no kernel driver
  bound (`/sys/bus/usb/devices/1-5:1.0/driver` does not exist).

Endpoints. The interface declares `bNumEndpoints 8`, not 4:

| Address | Dir | Type | wMaxPacketSize | bInterval |
| ------- | --- | ---- | -------------- | --------- |
| `0x01` | OUT | Bulk | 64 | 1 |
| `0x02` | OUT | Bulk | 64 | 1 |
| `0x03` | OUT | Bulk | 64 | 1 |
| `0x04` | OUT | Bulk | 64 | 1 |
| `0x81` | IN  | Bulk | 64 | 1 |
| `0x82` | IN  | Bulk | 64 | 1 |
| `0x83` | IN  | Bulk | 64 | 1 |
| `0x84` | IN  | Bulk | 64 | 1 |

Difference from `docs/protocol.md`: the protocol table lists four endpoints
(`0x01` OUT, `0x82`, `0x83`, `0x84` IN). All four exist on 0c90 at the stated
address and direction, all bulk. The device additionally exposes `0x81` IN and
`0x02`, `0x03`, `0x04` OUT, which the source says nothing about. No remap is
needed. Nothing is known about the four extra endpoints and nothing will be
sent to them.

`wMaxPacketSize` is 64 on every endpoint. `finger_info` (70 bytes), `commit`
(72 out) and `capture_start` (2*w*h) all cross that boundary, so the transport
read loop must reassemble. Confirms the plan.md Phase 1 requirement.

Unrecognised descriptor inside the vendor interface, before the endpoints:

```
09 21 10 01 00 01 22 15 00
```

That decodes as a HID class descriptor: bLength 9, bDescriptorType 0x21 (HID),
bcdHID 1.10, bCountryCode 0, bNumDescriptors 1, then type 0x22 (report)
length 0x0015 (21 bytes). A HID descriptor on an interface whose class is 255
is unusual. Not acted on. Recorded because it may explain how the Windows
driver talks to the chip.

### Correction to the 2026-09-14 endpoint entry

The entry above called `0x81`, `0x02`, `0x03` and `0x04` extra endpoints the
source does not mention. That framing was wrong. The eight endpoints are four
bidirectional pairs: `0x01`/`0x81`, `0x02`/`0x82`, `0x03`/`0x83`, `0x04`/`0x84`.
The protocol uses the OUT half of pair 1 and the IN halves of pairs 2, 3 and 4.
The four unused ones are the other halves of the same pairs, not separate
channels. The observed bytes in that entry are unchanged and correct.

## 2026-09-14 Task 0.2 device node permissions, before any udev rule

```
crw-rw-r-- 1 root root 189, 1 /dev/bus/usb/001/002
```

Owner root, group root, mode 0664. User `prawesh` (uid 1000) has read and no
write, so USB transfers are not possible as a non-root user right now. No
existing rule under `/etc/udev/rules.d` or `/usr/lib/udev/rules.d` mentions
`04f3`. The session is local and active (`loginctl`: `Remote=no`, `Active=yes`),
which is what `uaccess` requires.

`udevadm verify udev/70-elanmoc.rules` passes, 1 checked, 0 failed.

The rule is not installed. Installing it needs user approval.
