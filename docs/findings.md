# findings

What the ELAN `04f3:0c90` actually does, measured on real hardware.

Every byte below was observed on one unit, firmware 1.8, 80 x 80 sensor, with a
usbmon capture for each claim. Where the device disagreed with the source this
project started from, the device is recorded as right.

Hex is lowercase and space separated. `docs/protocol.md` is the command table;
this file is the evidence behind every `confirmed` in it.

---

## The device

From `lsusb -v -d 04f3:0c90`, saved in full as `docs/descriptors.txt`.

- `bcdUSB 2.00`, but the port negotiates **12 Mbit/s**, full speed.
- `bcdDevice 1.08`, which matches what `fw_ver` reports. `iSerial 0`, no serial.
- One configuration, one interface, `bInterfaceClass 255`, no kernel driver
  bound.

The interface declares **eight** bulk endpoints, not four. They are four
bidirectional pairs, `wMaxPacketSize` 64 and `bInterval` 1 on every one:

| Pair | OUT | IN | Used for |
| ---- | --- | -- | -------- |
| 1 | `0x01` | `0x81` | commands out; the IN half is unused |
| 2 | `0x02` | `0x82` | image data in; the OUT half is unused |
| 3 | `0x03` | `0x83` | status and data in |
| 4 | `0x04` | `0x84` | touch-wait results in |

The four "extra" endpoints the source does not mention are the unused halves of
those pairs, not separate channels. **Never write to `0x02`, `0x03` or `0x04`:**
they are OUT endpoints, so a write reaches firmware, and nothing is known about
what the firmware does with it.

`wMaxPacketSize` is 64 everywhere while `finger_info` is 70 bytes and `commit`
is 72 out, so the transport must reassemble across packets.

The vendor interface also carries a HID class descriptor,
`09 21 10 01 00 01 22 15 00`: HID 1.10, one 21 byte report descriptor. Unusual
on a class 255 interface. Not used here. It may be how the Windows WBF stack
binds.

---

## Four behaviours in no public source

These are the contribution. Each cost real time to find, and three of them will
silently corrupt or wrongly authenticate if an implementation guesses instead.

### 1. Touch-wait commands need arming first

A command that waits for a finger does not answer unless `enrolled_num`
(`40 ff 04`) was sent first **on the same claimed interface**. Without it the
chip accepts the command, acknowledges the OUT transfer, then sends nothing at
all, indefinitely, on any endpoint.

Observed four times: three silent, one answering.

Unarmed, reads posted on both documented IN endpoints in the same millisecond
as the command, touch confirmed by the user roughly four seconds later:

```
  5.896  S  bulk OUT 0x01  -115    3  40 ff 03
  5.896  S  bulk IN  0x83  -115   64
  5.896  S  bulk IN  0x84  -115   64
185.898  C  bulk IN  0x83    -2    0
185.899  C  bulk IN  0x84    -2    0
```

Every IN URB stayed pending for the full 180 seconds and ended at `-2`
(`ENOENT`), which is the host unlinking its own request, not a device error.
Zero bytes arrived. The OUT completed with status 0, so the command reached the
chip.

Identical bytes with the arming read in front:

```
 5.966  S  bulk OUT 0x01  -115    3  40 ff 04
 5.966  C  bulk IN  0x83     0    2  40 00
 5.967  S  bulk OUT 0x01  -115    3  40 ff 03
 5.967  S  bulk IN  0x83  -115   64
 5.967  S  bulk IN  0x84  -115   64
26.838  C  bulk IN  0x84     0    2  40 fd
26.838  C  bulk IN  0x03    -2    0
```

`40 fd` on `0x84` at touch latency. The endpoint theory was tested and rejected:
run 3 above posted reads on both IN endpoints and still got nothing, so silence
is not a wrong-endpoint artefact.

**To implement:** send `enrolled_num` once per claim before any touch-wait
command, and hold the claim across the whole sequence. Re-arming between
repeats is not needed; a 12.7 second pause mid-enrol cost nothing.

### 2. `finger_info` cannot tell an occupied slot from an empty one

The 0c4c source documents a 70 byte record per slot. On 0c90 every slot id
answers with **2 bytes, `40 ff`** whether it holds a template or not.

Probed ids 0 through 15 in one primed claim: all sixteen returned `40 ff`.

Then, after enrolling into slot 0 and confirming `enrolled_num` reads 1, slot 0
still answered `40 ff` — the same bytes as before anything existed.

The source reads `0xff` in byte 1 as a stuck sensor and prescribes a `verify` to
clear it. That was tried and changed nothing. On this chip `0xff` means "no
record to give you", not "stuck".

**Consequence:** no host can scan the chip to learn what is stored.
`enrolled_num` is the only reading that proves anything. An implementation that
picks an enrolment slot by scanning `finger_info` will be told every slot is
free and will overwrite a live template. Floor the slot choice at
`enrolled_num` instead.

One 70 byte occupied record was seen once, from slot 0, with the count at 1,
and never reproduced across roughly fifteen later reads in fresh and primed
claims. Recorded as an unexplained transient. A parser must accept both lengths;
one read still proves nothing.

### 3. `verify` carries no finger id

`verify` is `40 ff 03`: three bytes, no payload. The chip matches the touch
against **every template it holds** and answers with whichever one hit.

| Touch | Reply on `0x84` | Meaning |
| ----- | --------------- | ------- |
| finger enrolled at slot 0 | `40 00` | match, id 0 |
| finger enrolled at slot 1 | `40 01` | match, id 1 |
| a finger not enrolled | `40 fd` | no match |

Byte 0 is the `0x40` command echo, byte 1 carries the answer. `0xfd` is a normal
result, not a failure.

**Consequence, and this one is a security bug if missed:** asking to verify a
named finger is not something the protocol supports. A caller that names a
finger, or that names a user, must compare the returned id against the slots
that caller owns. Reporting the chip's raw answer as a match means any enrolled
template authenticates any request, including one belonging to a different
account, and including a template orphaned in flash by an uninstall.

### 4. The three IN endpoints are not interchangeable

`0x82` is image, `0x83` is status, `0x84` is touch-wait. Posting a read on the
wrong one does not fail; it hangs until the timeout.

Within a single enrol this alternates. The samples reply on `0x84`, the
collision check between them and the commit replies on `0x83`:

```
40 ff 01 00 08 07 00  ->  40 00     on 0x84   last sample
40 ff 10              ->  40 00 ff  on 0x83   collision check, 3 bytes
40 ff 11 f5 <69 zero> ->  40 00     on 0x83   commit
```

An enrol loop that assumes every queued command is a touch wait will read the
collision check on `0x84`, wait out the full sample timeout, and never send
commit. That cost this project two failed enrolments before it was found.

---

## Confirmed command behaviour

Eight of the sixteen commands in `docs/protocol.md` have been sent to this
device with the reply recorded.

| Command | Bytes out | Reply | On |
| ------- | --------- | ----- | -- |
| `fw_ver` | `40 19` | `01 08`, firmware 1.8 | `0x83` |
| `sensor_size` | `00 0c` | `4f 00 4f 00`, 80 x 80 | `0x83` |
| `enrolled_num` | `40 ff 04` | `40 00` / `40 01` / `40 02` | `0x83` |
| `finger_info` | `40 ff 12` + id | `40 ff`, 2 bytes, any id | `0x83` |
| `verify` | `40 ff 03` | `40 00` / `40 01` / `40 fd` | `0x84` |
| `enroll` | `40 ff 01` + 4 | `40 00` per accepted sample | `0x84` |
| `check_enrolled_collision` | `40 ff 10` | `40 00 ff`, 484 ms | `0x83` |
| `commit` | `40 ff 11` + 69 | `40 00`, 102 ms | `0x83` |

Notes that are not obvious from the table:

- **`sensor_size` has a real off-by-one.** Width is `byte0 + 1`, height is
  `byte2 + 1`. Bytes 1 and 3 are zero, consistent with two little endian 16 bit
  fields.
- **`enrolled_num` byte 0 is an echo**, not data. Byte 1 is the count.
- **`abort` (`40 ff 02`) returns nothing**, despite the source giving `in_len`
  2. Confirmed from an idle session with a 5 second wait. Do not wait for a
  reply. Send it before releasing the interface after any error, or the chip
  stays mid-session and the next command reads a stale response.
- **Enrol retries hold the counter.** `0x41`, `0x43` and `0x44` (move hints)
  resend the same attempt number rather than advancing it.
- **`check_enrolled_collision` byte 2 is `0xff` even with no collision.** Only a
  nonzero byte 1 names a clashing id, so byte 2 alone means nothing.
- **`commit` is clean.** `40 00` about 102 ms after the 72 byte write, no cancel
  needed, and a 500 ms trailing read returned nothing. The enrol bug named in
  the source did not reproduce here.
- A trailing read after a completed command returns the previous value, so a
  post-success read is not proof of anything new.

### Matching, measured

Ten separate primed claims against an enrolled finger: **10 of 10 matched** on
id 0. Ten more with a finger that was not enrolled: **10 of 10 answered
`40 fd`**, zero retry codes. A later run on a second template matched 5 of 5.

Touch latency varied from about 2 to 85 seconds, which is how long a person took
to present the finger, not chip time.

### Templates survive power loss

After a full machine reboot, with the sensor re-enumerating at a new device
address, `enrolled_num` read `40 01` before any touch and the finger matched
again. Templates are in the chip's flash, not in session state.

---

## Differences from the 0c4c source

`depau/elanpoc` (MIT) covers 0c00, 0c4c and 0c5e. It does not list 0c90.
Everything in `docs/protocol.md` began as a transcription from that family.

| Command | Expected from the source | Observed on 0c90 |
| ------- | ------------------------ | ---------------- |
| `fw_ver` | 2 bytes, major then minor | matches, `01 08` |
| `sensor_size` | 4 bytes, off-by-one | matches, 80 x 80 |
| `enrolled_num` | byte 1 is the count | matches |
| `finger_info` | 70 byte record per slot | **2 bytes, `40 ff`**, every id |
| `verify` | replies on `0x84` | matches, **but needs arming first** |
| `abort` | `in_len` 2 | **no reply at all** |

The arming rule and the endpoint alternation within an enrol are not in the
source in any form.

---

## Design consequences

Three rules follow from the findings above. Each one was learned by breaking it.

**The host's record must never drive a device erase.** A mapping file that
disagrees with the chip is resolved by trusting the chip. Deleting a template
requires an explicit human request and nothing else. A sync that erased flash to
match an empty host file is how a working template was lost here.

**Slot choice is floored at `enrolled_num`.** Because finding 2 means no scan can
see what is stored, the count is the only safe floor. The host file may rule
further slots out; it may never rule one back in.

**A returned id must be checked against the caller's own slots.** Finding 3 means
the chip answers with whichever template hit. Reporting that raw is a false
accept across users and across orphaned templates.

---

## Known unknowns

- **`total_attempts = 8` is borrowed from 0c4c.** Nothing on this chip reports a
  stage count, and the chip never echoes the total back. Eight completed end to
  end, so it is a working value, not a confirmed one.
- **The slot limit is unknown.** All sixteen probed ids answer identically, so
  the chip does not range-check the id in any way this probe can see. The only
  route the source offers is enrolling until `0xdd`, which fills the chip.
- **`wipe_all` has never been sent.** The bytes are transcribed; nobody has
  watched them run.
- **Everything here is one unit, one firmware.** The arming rule and `0xff`
  meaning "empty" may be revision specific. The firmware version is logged on
  the first claim so a difference is visible.
