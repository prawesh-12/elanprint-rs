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

## 2026-09-14 `verify` ANSWERS when `enrolled_num` precedes it in the same session

Probe 1. Touch confirmed by the user: flat pad, about 1 second, a few seconds
after the run started. Commands unchanged, both already in the table, sent in
the order the source's enroll sequence uses. Reads posted on `0x83` and `0x84`.

```
 5.966  S  bulk OUT 0x01  -115    3  40 ff 04
 5.966  C  bulk OUT 0x01     0    3
 5.966  S  bulk IN  0x03  -115   64
 5.966  C  bulk IN  0x03     0    2  40 00
 5.967  S  bulk OUT 0x01  -115    3  40 ff 03
 5.967  C  bulk OUT 0x01     0    3
 5.967  S  bulk IN  0x03  -115   64
 5.967  S  bulk IN  0x04  -115   64
26.838  C  bulk IN  0x04     0    2  40 fd
26.838  C  bulk IN  0x03    -2    0
```

**`verify` replied `40 fd` on `0x84`, 20.87 seconds after the command**, which
is the touch latency, not a protocol delay. Byte 0 is the `0x40` echo, byte 1 is
`0xfd`, "finger not enrolled", exactly what `docs/protocol.md` predicts with
`enrolled_num` at 0.

Three things are now settled:

- **Ordering was the cause.** The only difference from the three silent runs is
  that `enrolled_num` (`40 ff 04`) was sent first, in the same claim. No new
  bytes, no changed bytes, no new endpoint.
- **The documented endpoint mapping is correct.** The reply came on `0x84`, the
  touch-wait endpoint, while the `0x83` read posted alongside it was still
  pending and was unlinked at `-2`. The dual read earned its keep by proving
  this rather than assuming it.
- **The touch-wait path works.** `0x84` delivers, timeouts and cancellation
  behave, and the earlier silences were a missing precondition, not a broken
  read path.

### Q-002 resolved: empty slot, not a stuck sensor

`docs/protocol.md` says byte 1 of `0xff` means the sensor is stuck and that the
source's remedy is to run `verify`. `verify` has now genuinely run and returned
`0xfd`. `finger_info 0` afterwards still returns `40 ff`:

- in a fresh session: `40 ff`
- in a primed session, `enrolled_num` then `finger_info` in the same claim:
  `40 ff`
- all ten ids 0 to 9, primed: `40 ff`

The documented remedy was applied and changed nothing, so `0xff` here is not the
stuck state. With `enrolled_num` at 0, `0xff` in byte 1 of `finger_info` is
0c90's answer for a slot that holds nothing. Q-002 closes on the empty-slot
reading.

Note on method: the first `finger_info 0` after the successful `verify` ran as a
separate CLI process, so a fresh unprimed claim. That was not the test the user
asked for. It was re-run primed, in one session, and gave the same `40 ff`.

### Phase 2 confirmations re-checked after arming

`fw_ver` `01 08`, `sensor_size` `4f 00 4f 00` (80 x 80), `enrolled_num` `40 00`.
Identical to the values recorded before any `verify` had ever succeeded. The
config queries were not affected by the sensor being unarmed, so the Phase 2
confirmations stand as taken.

`enrolled_num` returned `40 00` again in the primed run, unchanged. Under the
byte 0 reading that is status 0 with count 0, and those two coincide at zero, so
this reply cannot yet separate the two fields. Anything other than `40 00` would
have distinguished them. It did not appear.

---

# The arming rule: touch-wait commands need `enrolled_num` first

**Date:** 2026-09-14. **Device:** ELAN 04f3:0c90, firmware 1.8, 80 x 80 sensor.
**Status:** observed four times, three negative and one positive, with a usbmon
capture for each.

This is the project's first novel result. It is written out in full because it
is not in `depau/elanpoc`, not in `docs/protocol.md`, and not in any published
description of this chip family that we are aware of.

## The rule

On 0c90, a command that waits for a finger does not answer unless
`enrolled_num` (`40 ff 04`) has been sent first **on the same claimed
interface**. Without it the chip accepts the command, acknowledges the OUT
transfer, and then sends nothing at all, indefinitely, on any endpoint.

With the arming read, the same command on the same endpoint answers normally at
touch latency.

## Evidence

### Three negative runs

`verify` (`40 ff 03`) sent immediately after open and claim, with no other
command in between.

| Run | Wait | Reads posted | Touch | Result |
| --- | ---- | ------------ | ----- | ------ |
| 1 | 120 s | `0x84` | not confirmed | nothing |
| 2 | 120 s | `0x84` | intended, unverifiable | nothing |
| 3 | 180 s | `0x83` **and** `0x84` | **confirmed by the user** | nothing |

In every case the OUT transfer on `0x01` completed with status 0, so the command
reached the chip. Every IN URB stayed pending for the full timeout and ended at
`-2` (`ENOENT`), which is the host unlinking its own request, not a device
error. Zero bytes arrived.

Run 3 matters most. Reads were posted on both documented IN endpoints in the
same millisecond as the command, roughly four seconds before the touch, and the
touch is confirmed. Capture:

```
  5.896  S  bulk OUT 0x01  -115    3  40 ff 03
  5.896  S  bulk IN  0x83  -115   64
  5.896  S  bulk IN  0x84  -115   64
185.898  C  bulk IN  0x83    -2    0
185.899  C  bulk IN  0x84    -2    0
```

### The positive run

Identical bytes, identical endpoints, one difference: `enrolled_num` first, in
the same claim.

```
 5.966  S  bulk OUT 0x01  -115    3  40 ff 04
 5.966  C  bulk IN  0x83     0    2  40 00
 5.967  S  bulk OUT 0x01  -115    3  40 ff 03
 5.967  S  bulk IN  0x83  -115   64
 5.967  S  bulk IN  0x84  -115   64
26.838  C  bulk IN  0x84     0    2  40 fd
26.838  C  bulk IN  0x03    -2    0
```

`40 fd` on `0x84`, 20.87 seconds after the command, which is how long the user
took to touch. Byte 0 is the `0x40` echo, byte 1 is `0xfd`, "finger not
enrolled", correct for a chip with `enrolled_num` at 0.

## Why the endpoint theory was tested and rejected

Before probe 1 the leading explanation was a wrong endpoint. `docs/protocol.md`
translates the endpoint split from the source's raw libusb numbers for 0c4c,
never confirmed on 0c90, and names a reply arriving on the wrong endpoint as a
suspected cause of the known enroll bug.

That theory could not be tested by capture alone. **usbmon records URBs, not
bus-level NAKs**: if the chip answered on an endpoint with no read posted, the
device would be NAKed and nothing would appear in the trace. Silence on `0x84`
therefore did not rule out a reply elsewhere.

Run 3 settled it by posting reads on `0x83` and `0x84` at once. Both stayed
silent. Then the positive run showed the reply arriving on `0x84` while the
`0x83` read posted beside it went unlinked at `-2`.

**The documented endpoint mapping is correct.** `0x84` is the touch-wait
endpoint on 0c90, exactly as the table says. The three IN endpoints must stay
distinct.

## The rule to implement

From D-012:

- Send `enrolled_num` (`40 ff 04`) once, at the start of a session, before any
  touch-wait command.
- Hold **one claim** for the whole sequence. Do not open, claim and release per
  command.
- This costs one 3 byte command and one 2 byte reply, well under a millisecond.

## Why this may be the known enroll bug

Untested, stated as a hypothesis and labelled as one.

The reported symptom of the community bug is that enrol appears to finish, then
errors, and the print is saved only if the user cancels. `docs/protocol.md`
gives `enroll` the same touch-wait shape as `verify` on the same `0x84`.

A host that opens a fresh session per step, or that skips the arming read, would
see a touch-wait command that never answers, and would report its own timeout as
a failure while the chip had in fact done the work. That matches the symptom.

It is not claimed as the cause. Phase 3 will test it deliberately rather than
letting the arming rule fix things as a side effect, so the evidence is clean
enough to publish.

## What the rule does not explain

- Why the chip behaves this way. Nothing here shows a mechanism.
- Whether commands other than `verify` need arming. Only `verify` was tested.
  `enroll` is untested.
- Whether some command other than `enrolled_num` would also arm it. Not probed,
  and not worth probing by trial on a flash-capable MCU.
- Whether the config queries need it. They do not: `fw_ver`, `sensor_size` and
  `enrolled_num` return identical values armed or unarmed.

---

## 2026-09-14 Slot limit probe, ids 0 to 15

`finger_info` for ids 0 through 15, primed, all in one claim. **All sixteen
returned `40 ff`.**

No id in that range answers differently, so the chip does not range-check the
finger id in a way this probe can see, and **the slot limit stays unknown**.
No writes were made and nothing was enrolled.

The only route to the limit that `docs/protocol.md` offers is enrolling until
`0xdd`, which fills the chip. Not done, on the user's instruction.

---

## 2026-09-14 GATE 3a: first enroll, slot 0, commit is clean

One claim, armed once with `enrolled_num`, never re-armed. Capture
`/tmp/enroll3a.pcapng` (bus 1, device 2, dumpcap as uid 1000) and
`RUST_LOG=elanmoc_usb=debug` log agree on every byte. Whole run took 22 seconds.

Method note, stated plainly: the user was at the sensor and touching throughout
(move-hint retries and sub-second captures prove a live finger), but no
per-touch handshake message was exchanged for any of the nine touches, so every
touch in this run is logged as **unconfirmed** per D-011. The commit, collision
and count evidence below does not depend on touch timing.

Sequence on the wire:

```
arm:       OUT 0x01  40 ff 04            IN 0x83  40 00 (count 0)
slot check: OUT 0x01  40 ff 12 00        IN 0x83  40 ff
pre-check:  OUT 0x01  40 ff 03            IN 0x84  40 fd (148 ms)
samples:    OUT 0x01  40 ff 01 00 08 XX 00  (XX = 00..07, one claim)
            IN 0x84  40 00 x8, plus 40 43, 40 44, 40 41 retries
collision:  OUT 0x01  40 ff 10            IN 0x83  40 00 ff
commit:     OUT 0x01  40 ff 11 f5 + 68 zeros (72 bytes)
                                          IN 0x83  40 00 (102 ms)
after:      enrolled_num -> 40 01 (was 40 00)
            finger_info 0 -> 40 ff (still 2 bytes)
```

Facts:

- **Commit works clean, n=1, provisional.** `40 00` on `0x83`, 102 ms after the 72 byte write. A
  500 ms trailing read on `0x83` got nothing (capture shows our own unlink at
  `-2`). No cancel, no workaround, no second command. This is one enroll into
  an empty chip (slot 0, sub id `0xf5`, no collision possible), so it tests
  neither suspect in `docs/protocol.md`: the `0xf0 | (id + 5)` nibble nor the
  count-as-id allocation. The bug is not recorded as fixed. The second enroll
  decides whether the arming rule plus a held claim accounts for it.
- **Attempt counters 00 to 07 all accepted.** Three retries (`0x43`, `0x44`,
  `0x41`) held the counter at 04 and resent the same attempt, exactly as the
  state machine does. A 12.7 second pause at attempt 04 (finger adjustment)
  cost nothing: no re-arm, no desync, the next sample answered normally.
- **Collision reply is `40 00 ff`.** Byte 1 is 0 (no clash), byte 2 is `0xff`.
  So byte 2 carries `0xff` even when there is no collision, and only a nonzero
  byte 1 would name a clashing id.
- **`finger_info` cannot find the enrolled finger.** After commit,
  `enrolled_num` reads 1 but `finger_info 0` still answers 2 bytes `40 ff`,
  the same bytes as before anything was enrolled. The Phase 5 warning is now
  proven live: no slot scan can locate the occupied slot, and sync must use the
  count, not the records.
- `total_attempts` 8 completed end to end. The chip never echoes the total back,
  so 8 stays a working value rather than a confirmed one.

Promotions: `enroll`, `check_enrolled_collision` and `commit` go to `confirmed`
with the responses above. `total_attempts` 8 is not promoted beyond working.

---

## 2026-09-14 Phase 4: 10 of 10 verifies match, handshake each run

Ten separate `verify --prime` claims against the enrolled right-index-finger.
D-011 followed for every run: the run started in the background, the user was
asked to touch, the reply was read only after the user's message.

All ten answered `40 00` on `0x84`, match on finger id 0. Touch latencies in
seconds: 6.08, 17.86, 5.20, 4.77, 5.00, 11.92, 5.10, 7.88, 15.03, 48.40.

Two runs do not count and are recorded so the count stays honest:

- One run answered `40 00` after 89.27 seconds with no message from the user.
  Unconfirmed touch, excluded from the 10.
- One reject-series run (different finger, expecting `40 fd`) was started and
  then killed untouched at the user's order before any touch. It sent only the
  arming read and the verify command. A following `info` returned the earlier
  values, so the killed wait left no desync.

The template is real: it matches, not just commits. The different-finger
reject series (10 runs expecting `0xfd`) is still open. Stopped at 10 of 10
matches on the user's order.

---

## 2026-09-14 Daemon serves on the session bus, property names fixed

`elanmocd` ran unprivileged with `ELANMOC_BUS=session` and answered a remote
`busctl --user` sequence: `GetDevices` lists Device/0, `Claim` opens and arms
(`enrolled_num` `40 01`, count persists from GATE 3a), `num-enroll-stages`
reads 8 while claimed and -1 after `Release`.

Two corrections from the smoke run:

- zbus renders `num_enroll_stages` as `NumEnrollStages`, but fprintd's live
  interface names it `num-enroll-stages`. All five properties now carry
  explicit dash names and introspect exactly as `docs/dbus-device.xml`.
- `ListEnrolledFingers` for a user with no store entry replies with the
  correct error name `net.reactivated.Fprint.Error.NoEnrolledPrints` on the
  wire (busctl renders unknown error names as "Input/output error", which is
  a display artifact, not a daemon bug).

One transient, stated as observation: a `Claim` re-arm on the held handle
timed out once after about 60 seconds idle and succeeded on immediate retry
(`40 01` in about 1 ms). Autosuspend is a hypothesis, not established. The
daemon returns the timeout as-is with no retry.

Also fixed: zbus must run on the tokio runtime (`features = ["tokio"]`).
On its default executor, interface methods panic with "no reactor running"
the moment a USB transfer sleeps.

---

## 2026-09-14 Live sync against count 0, corrupt store pruned

Before GATE 3a, device count 0. `elanmoc-cli sync --store` with a missing
file: "store holds nothing". With `{"alice":{"right-index-finger":1}}`:
reported stale ("device holds nothing, enrolled_num is 0"), and `--prune`
removed it, leaving `{}`. Ambiguous-slot behavior is unit tests only until a
second slot is occupied.

---

## 2026-09-14 Transient 70 byte finger_info, seen once

During the audit fixes, one primed `finger_info 0` returned a 70 byte
occupied record (`40 00` plus 68 zeros) with the count at 1. Every other
read of slot 0 before and since, about fifteen in fresh and primed claims,
returned 2 bytes `40 ff`.

Reproduction tries, all read-only, all negative: idle abort then read,
daemon claim plus release (which now sends abort) then immediate read.
The single hit came right after a daemon release carrying the first
abort this daemon ever sent, but the same sequence does not reproduce it.

Stated as a transient with unknown mechanism, not a finding. Leading
hypothesis, labeled as one: reply length depends on chip session history,
same family as the arming rule. The parser already accepts both forms, so
no code changes. The sync rule stands: one read never proves a slot state.

---

## 2026-09-14 Phase 4 rejects: 10 of 10 no-match, handshake each run

Ten separate primed `verify` claims with a non-enrolled finger, D-011 for
every run: backgrounded, touch asked, log read only after the message.

All ten answered `40 fd` on `0x84`, no-match. No retry code in any run, so
nothing was re-run and nothing was excluded. Touch latencies in seconds:
27.28, 8.76, 7.79, 7.66, 9.59, 6.18, 2.52, 2.32, 8.00, 2.03.

Phase 4 is fully complete: 10 of 10 matches on id 0 plus 10 of 10
no-matches on other fingers. The no-match path in the UI and the daemon
mapping now rest on observed `0xfd`, not on assumption.

---

## 2026-09-14 Slot 1 enroll FAILED at the collision check, no flash write

Same discipline as GATE 3a: one armed claim, capture running, handshake per
touch, hold before commit (commit never sent). Left index finger, slot 1,
sub id `0xf6`.

Accepted: pre-check `0xfd` (confirmed), 8 samples `40 00` (6 confirmed, 2
arrived on lingering touches and logged unconfirmed), 2 `MoveUp` retries at
attempt 6 held the counter and resent correctly.

Then `40 ff 10` went out and nothing came back in 2 s. Timeout, abort sent,
bailed before commit. No flash write: count stayed 1. Capture holds the
whole run plus the abort.

Candidates, undecided from one event: the 2 s collision timeout is marginal
(GATE 3a answered in about 1.07 s); the minutes-long messaging gaps between
touches here versus 22 s straight in GATE 3a may have starved the session,
though the last sample answered one read prior; or a transient wedge.
Post-abort the first `fw_ver` read stale (`40 19` answered `40 00`), the
next claim read clean (1.8, 80x80, count 1). Self-recovered, no reset sent.

No retry. A second attempt, if ordered, wants a longer collision timeout
and tighter touch turnaround, stated as a plan change first.

---

## 2026-09-14 Slot 0 lost: what the surviving artifacts say

Read-only `info` on the current session: fw 1.8, 80 x 80, `enrolled_num`
`40 00`, count 0. The slot 0 template recorded at GATE 3a is gone.

No capture from the window survives (`captures/` is gitignored and empty,
and the two `tools/run.sh` sessions logged to a terminal, not to a file).
What follows is built from files that do survive. It is an evidence chain,
not a byte record, and is labelled as such.

Daemon logs in `/tmp`, all timestamps UTC, local is +0530:

| Log                    | Time     | Arming read       | Count |
| ---------------------- | -------- | ----------------- | ----- |
| `elanmocd-audit.log`   | 15:24:59 | `40 ff 04` `40 01` | 1     |
| `elanmocd-step6.log`   | 16:29:23 | `40 ff 04` `40 01` | 1     |
| `elanmocd-diag.log`    | 17:11:59 | `40 ff 04` `40 00` | 0     |

So the count went 1 to 0 between 16:29:23 and 17:11:59 UTC (21:59 and 22:41
local). `tools/run.sh` ran across that window, 22:00:33 to about 22:20
local, from the shell history.

`/tmp/elanmoc-prints.json`, the store that run uses, holds `{}` with mtime
22:13 local, inside the window. An empty object is a saved file, not a
missing one. Three code paths write the store: the enroll completion, which
saves a non-empty map; `sync --prune`, which was not run; and
`Worker::delete_finger`, which sends `delete` (`40 ff 05 <slot> 00`) and then
saves the map with the entry removed. The store held
`prawesh/right-index-finger` at slot 0 before that run.

The build in that window is commit `80fa393`. In it, `Device::start_op` took
the busy flag and `Worker::enroll` / `Worker::verify` took it again, so every
spawned enroll and verify returned `Busy` before sending a single byte, and
the error was dropped by `let _ =`. `delete_enrolled_finger` does not go
through `start_op`. Delete was therefore the only device operation in that
build that could reach the chip.

Conclusion, stated at the strength the evidence carries: the template was
erased by `delete`, the one path still working, and the store save at 22:13
is the same operation's second half. It was not lost to a firmware fault, a
failed enroll, or a `wipe_all`: `wipe_all` has no call site anywhere in the
workspace, and enroll could not reach the wire.

### The store-to-device sync theory, tested

Ruled out as the cause. `Worker::delete_finger` reads the slot from the store
and returns `NotEnrolled` when there is no entry, so an empty store cannot
produce an erase. `elanmoc-cli sync --prune` writes the store only; it has no
device-write path and the CLI has no delete subcommand at all. The direction
of the dependency is the opposite of the theory: the store entry is what
*enabled* the delete.

### A second, real sync-direction fault, found in the enroll path

`free_slot` scanned ids from 0 and treated the store as the record of which
slots are occupied, with `finger_info` as a veto. On 0c90 that veto never
fires: an occupied slot answers `40 ff`, the same two bytes an empty one
gives, already recorded above under `finger_info`. With an empty store, which
is the normal state for the root daemon (`/var/lib/elanmoc` does not exist on
this machine, so no save has ever succeeded there), `free_slot` returned 0 on
every call. The next successful enroll would have written over slot 0 to match
an empty file.

Not observed on hardware: nothing reached the enroll path in that build. It is
read off the code plus the confirmed `40 ff` behaviour, and is recorded as a
structural fault, not an event.
