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

## 2026-09-14 Task 0.2 udev rule installed

Installed with `tee` (`cp` is not permitted). `/etc/udev/rules.d/70-elanmoc.rules`
md5 `196afa25e9050c4d2eaa7913057b6251`, identical to the repo copy.

Before: `crw-rw-r-- root root 0664`, no ACL.
After `udevadm control --reload-rules` plus the targeted add trigger:

```
crw-rw----+ 1 root root 189, 1 /dev/bus/usb/001/002
user::rw-
user:prawesh:rw-
group::rw-
mask::rw-
other::---
```

`uaccess` applied on the first add trigger. No reboot, no replug, no rule
variant. The earlier failure was that the file had never reached
`/etc/udev/rules.d` at all, not anything about the rule's content.

## 2026-09-14 Task 0.3 usbmon loaded, captures need root

`sudo modprobe usbmon` succeeded, module resident. Nodes exist:

```
crw------- 1 root root 505, 0 /dev/usbmon0
crw------- 1 root root 505, 1 /dev/usbmon1
crw------- 1 root root 505, 2 /dev/usbmon2
```

`0600 root:root`. Reading `/dev/usbmon1` as uid 1000 gives EACCES.

`wireshark-common 4.2.2-1.1build3` is installed, user is in group `wireshark`
(gid 136), and `/usr/bin/dumpcap` is `root:wireshark 0750` with
`cap_net_admin,cap_net_raw=eip`. `dumpcap -D` lists usbmon0/1/2, but an actual
capture fails: those capabilities do not include `CAP_DAC_OVERRIDE`, so the
0600 node still blocks the open. No udev rule ships to relax usbmon permissions
on this system.

Consequence: usbmon captures require root. Per the user, Phase 2 proceeds
without them. The parsed response is compared against `docs/protocol.md` only,
with no independent wire view. Any disagreement gets recorded here rather than
resolved against a capture.

## 2026-09-14 Phase 2, four read-only commands confirmed on 0c90

No usbmon capture (needs root, see above). Evidence is the transport's own hex
log of each transfer, at `RUST_LOG=elanmoc_usb=debug`. Every value below was
read three or more times and did not change.

### fw_ver

```
OUT ep 0x01  40 19
IN  ep 0x83  01 08
```

Two bytes, as documented. Major 1, minor 8, so firmware 1.8.

Corroboration: `bcdDevice` in the device descriptor is `0108`. The command's
answer and the descriptor agree, which makes a coincidence unlikely. This is a
real version, not garbage. GATE 2 is GO.

### sensor_size

```
OUT ep 0x01  00 0c
IN  ep 0x83  4f 00 4f 00
```

Four bytes, as documented. `0x4f + 1 = 80` both times, so the sensor is
80 x 80. The documented off-by-one is present: without it the value would be
79 x 79, and a 79 pixel sensor is not a plausible number. Bytes 1 and 3 are
zero, consistent with two little endian 16 bit fields.

### enrolled_num

```
OUT ep 0x01  40 ff 04
IN  ep 0x83  40 00
```

Byte 1 is the count, as documented: 0. No fingers are enrolled on this device.
Byte 0 is `0x40`, which is the first byte of the command. Every status reply
seen so far starts `0x40`, so byte 0 looks like an echo of the command class
rather than data.

### finger_info, ids 0 to 9

```
OUT ep 0x01  40 ff 12 00      (through 40 ff 12 09)
IN  ep 0x83  40 ff
```

**Difference from the documented source.** The table says `in_len` is 70.
0c90 returned **2 bytes, `40 ff`**, for every id from 0 to 9, ten out of ten.
The transport reported a short read and kept the bytes, so nothing was lost.

Per `docs/protocol.md` the two byte form means byte 1 is an error code, and
`0xff` in byte 1 separately means the sensor is in a stuck state. Both readings
apply to `40 ff` and this run cannot tell them apart. The plainer reading, given
`enrolled_num` is 0 and all ten ids answered identically, is that 0c90 answers
`40 ff` for a slot that holds nothing, where 0c4c returns a 70 byte record whose
last byte is `0xff`. That is a guess about meaning, not an observation, and it
is recorded as such. See Q-002.

The parser was not changed to make 70 bytes appear. It reports what arrived.

### Session hygiene

Ten `finger_info` commands in a row, then `fw_ver`, `sensor_size` and
`enrolled_num` again: all three returned their earlier values. No desync, no
stale response, and `abort` was never needed because nothing failed.

## 2026-09-14 usbmon readable without root

Appended to the same rules file:

```
SUBSYSTEM=="usbmon", GROUP="wireshark", MODE="0640"
```

`/dev/usbmon0..2` went from `0600 root:root` to `crw-r----- root wireshark`.
`id -nG` already contained `wireshark`, so no re-login was needed. `dumpcap`
captured 713 packets with 0 dropped as uid 1000.

`tshark` is **not installed** on this machine (only `dumpcap`, `capinfos` and
the Wireshark GUI), and installing it needs `apt`, which is outside the allowed
sudo commands. `tools/usbmon_filter.py` reads the pcapng and decodes the Linux
usbmon mmapped header (DLT 115, 64 byte header) instead. It takes a bus and
device number and prints one line per URB.

## 2026-09-14 verify with no touch, captured

First use of the `0x84` touch-wait path. `verify` was sent with a 120 second
wait. **Nobody is known to have touched the sensor during the window**, so this
run says nothing about what a touch produces. It is recorded for what it does
show.

Capture, bus 1 device 2, times relative to the first packet:

```
  5.624  S  bulk OUT 0x01  -115    3  40 ff 03
  5.625  C  bulk OUT 0x01     0    3
  5.625  S  bulk IN  0x84  -115   64
125.626  C  bulk IN  0x84    -2    0
125.626  S  bulk OUT 0x01  -115    3  40 ff 02
125.626  C  bulk OUT 0x01     0    3
125.627  S  bulk IN  0x83  -115   64
126.628  C  bulk IN  0x83    -2    0
```

What this establishes:

- `40 ff 03` was accepted on `0x01`, completion status 0, 3 bytes written. The
  command reached the chip.
- The IN URB on `0x84` was submitted 1 ms later and stayed pending for the full
  120 seconds. Completion status `-2` is `ENOENT`, the URB being unlinked by our
  own timeout, not a device error. The chip sent nothing.
- Cancellation works on the wire: the timeout cancelled a pending URB cleanly.
- `abort` (`40 ff 02`) was also accepted on `0x01` with status 0.

What this does **not** establish: nothing about `0xfd`, nothing about whether a
touch produces a reply on `0x84`, and nothing about Q-002. A touch-wait command
that is never touched is expected to return nothing. Q-002 stays open.

### `abort` produced no reply within 1 second

`docs/protocol.md` gives `abort` `in_len` 2 on `0x83`. The IN URB on `0x83` was
still pending when our 1 second timeout unlinked it. Three explanations fit and
this run cannot separate them: `abort` returns nothing on 0c90, the chip was
still inside the unfinished `verify` session, or 1 second is too short. Not
concluded. `abort` stays at `documented`.

### No desync

After the abandoned `verify` and the unanswered `abort`, `fw_ver`,
`sensor_size` and `enrolled_num` all returned their earlier values:
`01 08`, `4f 00 4f 00`, `40 00`. The session did not wedge.

## 2026-09-14 `abort` returns nothing on 0c90

Sent from an idle session with nothing pending and a 5 second wait, captured:

```
2.006  S  bulk OUT 0x01  -115    3  40 ff 02
2.006  C  bulk OUT 0x01     0    3
2.007  S  bulk IN  0x03  -115   64
7.008  C  bulk IN  0x03    -2    0
```

`40 ff 02` was accepted on `0x01`, completion status 0. The IN URB on `0x83`
stayed pending the full 5 seconds and ended at `-2`, our own unlink. The chip
sent nothing.

**Difference from the documented source.** `docs/protocol.md` gives `abort`
`in_len` 2 on `0x83`. 0c90 replies with **0 bytes**. The `in_len` column is
left at 2 on the user's instruction; this entry is the record.

This settles the ambiguity from the earlier `verify` run. The silence there was
not the unfinished `verify` session holding the chip: `abort` is silent from
idle too.

Consequence, and it matters: any error path that waits for `abort`'s reply
burns its whole timeout and then reports a failure that did not happen. Before
this change, every failed command cost an extra second and printed
"abort failed: transfer timed out". `Command::Abort::expected_len()` is now 0,
so `abort` is send-only and returns in about 360 microseconds. `Response::Abort`
carries `Option<Status>` so a reply is still parsed if some other firmware
sends one.

`fw_ver` and `enrolled_num` unchanged afterwards.

## 2026-09-14 Second `verify` attempt, still no reply on `0x84`

Capture, bus 1 device 2:

```
  5.758  S  bulk OUT 0x01  -115    3  40 ff 03
  5.758  C  bulk OUT 0x01     0    3
  5.758  S  bulk IN  0x84  -115   64
125.761  C  bulk IN  0x84    -2    0
125.761  S  bulk OUT 0x01  -115    3  40 ff 02
125.761  C  bulk OUT 0x01     0    3
125.774  S  bulk OUT 0x01  -115    4  40 ff 12 00
125.774  C  bulk OUT 0x01     0    4
125.775  S  bulk IN  0x83  -115  128
125.775  C  bulk IN  0x83     0    2  40 ff
```

Identical to the first attempt: `40 ff 03` accepted, the `0x84` URB pending the
full 120 seconds, unlinked by our own timeout at `-2`, zero bytes from the chip.

**Whether the sensor was touched during this window is not known.** The user
intended to be at the sensor. A touch produces no record in this capture or
anywhere else the agent can read, so the run cannot distinguish "touched and the
chip stayed silent" from "not touched". It is recorded as undetermined. Q-002
stays open, and no conclusion is drawn about the `0x84` path.

Two things this run does show:

- The `abort` fix is visible on the wire. At t+125.761 `40 ff 02` goes out with
  no IN URB submitted after it. The earlier runs submitted one and waited.
- The chip is responsive immediately afterwards. `finger_info 0` went out 13 ms
  after `abort` and answered `40 ff` in under a millisecond, from the same
  process, on the same claim. A 120 second unanswered `verify` does not wedge
  the session.

### Limit of this method

usbmon records URBs, not bus-level NAKs. If 0c90 answered `verify` on an
endpoint with no URB pending, the device would be NAKed and **nothing would
appear in the capture**. So these captures cannot rule out a reply arriving on
an endpoint other than `0x84`. Only `0x84` had a read posted.

## 2026-09-14 `verify` with a CONFIRMED touch, reads on both `0x83` and `0x84`

The first `verify` run whose touch is confirmed. User touched the sensor with a
flat pad, held about 1 second, a few seconds after the run started. Reads were
posted on `0x83` and `0x84` simultaneously before the touch.

**Raw bytes received: none. Neither endpoint answered.**

Capture, bus 1 device 2:

```
  5.896  S  bulk OUT 0x01  -115    3  40 ff 03
  5.896  C  bulk OUT 0x01     0    3
  5.896  S  bulk IN  0x83  -115   64
  5.896  S  bulk IN  0x84  -115   64
185.898  C  bulk IN  0x83    -2    0
185.899  C  bulk IN  0x84    -2    0
185.899  S  bulk OUT 0x01  -115    3  40 ff 02
185.899  C  bulk OUT 0x01     0    3
185.911  S  bulk OUT 0x01  -115    4  40 ff 12 00
185.911  C  bulk OUT 0x01     0    4
185.911  S  bulk IN  0x83  -115  128
185.911  C  bulk IN  0x83     0    2  40 ff
```

Facts:

- `40 ff 03` was accepted on `0x01`, completion status 0, 3 bytes written.
- Both IN URBs were submitted at t+5.896, in the same millisecond as the
  command and roughly 4 seconds before the touch. Neither was posted late.
- Both stayed pending for the full 180 seconds and ended at `-2`, our own
  unlink, with 0 bytes. The chip sent nothing on either endpoint.
- `finger_info 0` answered `40 ff` on `0x83` in under a millisecond, 12 ms
  after `abort`. The chip was alive and responsive the whole time.

What this rules out: the endpoint mapping is not the explanation. A reply on
`0x83` would have been caught, because a read was posted there. The
`0x84`-only silence of the two earlier runs was not a wrong-endpoint artefact.

What this does not establish: nothing about `0x81` or `0x82`, where no read was
posted, and nothing about why. Per the user, stopped here. No third endpoint
tried, no variation of the command.

### Consequence for Phase 3

`docs/protocol.md` has `enroll` replying on `0x84`, the same endpoint and the
same touch-wait shape as `verify`. `verify` produced nothing there on a
confirmed touch. There is no evidence yet that any touch-wait command on this
chip answers at all, so the enroll sequence cannot be assumed to work as
documented. GATE 3a is not approached.

### Q-002 still unresolved

`finger_info 0` still returns `40 ff` after the touch. The source's workaround
for a stuck sensor is to run `verify`, and `verify` was run, but since it
produced no reply it cannot be said to have been performed. Empty slot versus
stuck sensor is still undecided.
