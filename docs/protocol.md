
# ELAN 04f3:0c90 protocol

**SOURCE OF TRUTH. Nothing gets sent to the device unless it is in this file.**

Status values:

- `unconfirmed` - bytes exist but have never been run on this device. **Do not send.**
- `documented` - bytes come from a verified source. Safe to send.
- `confirmed` - sent on this exact device, real response recorded, layout verified.

An agent may only send commands at `documented` or `confirmed`.

**Primary source:** `depau/elanpoc` (MIT), a PoC for ELAN 04f3:0c4c. Its device list covers 0c00, 0c4c and 0c5e. It does **not** list 0c90. Everything below is therefore `documented` from that family, and must be promoted to `confirmed` one command at a time on this device per Phase 2 of plan.md.

---

## Device

- VID:PID `04f3:0c90`
- Product string `ELAN:ARM-M4`
- Full speed (12 Mbit/s)
- Interface 0, vendor specific, no kernel driver bound
- Claim interface 0 before any transfer

## Endpoints

Confirmed on 0c90 from `docs/descriptors.txt`. Interface 0 declares **eight** bulk endpoints, all `wMaxPacketSize` 64, `bInterval` 1. They are four bidirectional pairs:

| Pair | OUT      | IN       | Used for                                            |
| ---- | -------- | -------- | --------------------------------------------------- |
| 1    | `0x01` | `0x81` | commands out. IN half unused by the known protocol. |
| 2    | `0x02` | `0x82` | image data in. OUT half unused.                     |
| 3    | `0x03` | `0x83` | status and data in. OUT half unused.                |
| 4    | `0x04` | `0x84` | touch-wait results in. OUT half unused.             |

So the driver uses `0x01` OUT and `0x82` / `0x83` / `0x84` IN. The other four are the unused halves of the same pairs, not separate undocumented channels.

**Never write to `0x02`, `0x03` or `0x04`.** Nothing is known about what the firmware does with data on those, and they are OUT endpoints, which means a write reaches firmware. Reading `0x81` is harmless but pointless.

`wMaxPacketSize` is 64 on every endpoint. `finger_info` (70 bytes) and `capture_start` both exceed that, so the read path must reassemble across packets.

### HID descriptor

The vendor interface carries a HID class descriptor: `09 21 10 01 00 01 22 15 00`. That is HID 1.10, one report descriptor, 21 bytes long. Unusual on a class-255 interface.

Not used by this driver. It may be how the Windows WBF stack binds. Reading the report descriptor is a standard EP0 `GET_DESCRIPTOR`, the same class of operation `lsusb` performs, so it is safe to read if it ever becomes relevant. It is not relevant now.

## Framing

- Commands are short byte strings starting `0x40` for most operations. Image and sensor commands start `0x00` or `0x02`.
- Fixed-length request and response per command. No length field on the wire. You must know `in_len` in advance, which is why the table below is mandatory.
- In status responses, **byte 1 is the status/error byte.** Byte 0 varies by command.
- Error convention: if the high nibble of the status byte is zero, it is not an error. Otherwise look it up in the error table.

---

## Commands

`out_len` is the total bytes written, opcode plus payload. `in_len` is the exact bytes to read back.

| Name                         | Opcode (hex)                | out_len | in_len  | EP in    | Destructive            | Status     |
| ---------------------------- | --------------------------- | ------- | ------- | -------- | ---------------------- | ---------- |
| `fw_ver`                   | `40 19`                   | 2       | 2       | `0x83` | no                     | **confirmed** |
| `sensor_size`              | `00 0c`                   | 2       | 4       | `0x83` | no                     | **confirmed** |
| `enrolled_num`             | `40 ff 04`                | 3       | 2       | `0x83` | no                     | **confirmed** |
| `enrolled_num1`            | `40 ff 00`                | 3       | 2       | `0x83` | no                     | documented |
| `finger_info`              | `40 ff 12`                | 4       | 70      | `0x83` | no                     | **confirmed** |
| `verify`                   | `40 ff 03`                | 3       | 2       | `0x84` | no                     | documented |
| `abort`                    | `40 ff 02`                | 3       | 2       | `0x83` | no                     | documented |
| `enroll`                   | `40 ff 01`                | 7       | 2       | `0x84` | writes flash           | documented |
| `check_enrolled_collision` | `40 ff 10`                | 3       | 3       | `0x83` | no                     | documented |
| `commit`                   | `40 ff 11`                | 72      | 2       | `0x83` | **writes flash** | documented |
| `delete`                   | `40 ff 05`                | 5       | 2       | `0x83` | **erases**       | documented |
| `delete_subsid`            | `40 ff 13`                | 72      | 2       | `0x83` | **erases**       | documented |
| `wipe_all`                 | `40 ff 99`                | 3       | 0       | none     | **ERASES ALL**   | documented |
| `reset_device`             | `40 27 57 44 54 52 53 54` | 8       | 0       | none     | no                     | documented |
| `capture_start`            | `00 09`                   | 2       | 2*w*h | `0x82` | no                     | documented |
| `read_register`            | `40` + `(0x40 + reg)`   | 2       | 2       | `0x83` | no                     | documented |

Notes per command:

### `fw_ver`

Response is two bytes: major, minor. Use this as the Phase 2 GO/NO-GO probe.

**Observed on 0c90, 2026-09-14:** `01 08`, so firmware 1.8. Matches `bcdDevice
0108` in the device descriptor.

### `sensor_size`

Response is 4 bytes. Width is `byte0 + 1`, height is `byte2 + 1`. The off-by-one is real, not a typo.

**Observed on 0c90, 2026-09-14:** `4f 00 4f 00`, so 80 x 80. Bytes 1 and 3 are
zero, consistent with two little endian 16 bit fields.

### `enrolled_num`

Byte 1 of the response is the count of enrolled fingers.

**Observed on 0c90, 2026-09-14:** `40 00`, so 0 enrolled. Byte 0 is `0x40`,
the command's first byte, so it reads as an echo rather than data. **This is the one to use.** `enrolled_count` in plan.md and in the CLI refers to this command; the two names mean the same thing.

### `enrolled_num1`

Present in the source's command table but never called by it. Purpose and response layout unknown. `status: documented` here means the bytes are transcribed accurately, not that the command is understood. **Do not send it.** If you have a reason to, that is a QUESTIONS.md entry first.

### `finger_info`

Payload is a single byte finger id. Response is 70 bytes.

**Observed on 0c90, 2026-09-14:** ids 0 to 9 all returned **2 bytes, `40 ff`**,
never 70, with `enrolled_num` at 0. Read the two byte form before assuming a 70
byte one. See Q-002 for what `40 ff` means here, which is not yet settled.

 If byte 1 comes back `0xff`, the sensor is in a stuck state and the source works around it by running a `verify` to clear it. If the response is only 2 bytes long, treat byte 1 as an error code. The last byte being `0xff` means that slot is not enrolled.

### `verify`

Waits for a touch, so use a long timeout and read on `0x84`. Byte 1 is the matched finger id on success. `0xfd` means the finger is not enrolled, which is a **normal expected value**, not a failure, during the pre-enroll check.

### `enroll`

Payload is 4 bytes: `new_finger_id`, `total_attempts`, `attempts_done`, `0`. The source uses 8 total attempts. Called in a loop, once per touch. Byte 1 of the response is 0 on a good sample, otherwise an error code. `0xdd` means the slot limit is reached, and the loop must stop rather than retry.

### `check_enrolled_collision`

Run after the last sample, before commit. 3 byte response. If byte 1 is nonzero, byte 2 holds the id of the finger this one collides with, and enrollment must abort.

### `commit`

72 bytes total: 3 byte opcode plus a 69 byte payload. The payload is one "sub id" byte followed by user data, right padded with zeros to 69.

The sub id byte is computed as `0xf0 | (finger_id + 5)`.

**This is the step that is known broken.** Two concrete things to check on 0c90:

1. That sub id formula overflows the low nibble once `finger_id + 5 > 15`, so it breaks for finger id 11 and up. On a sensor with few slots you may never hit it, but the formula is clearly a guess at an encoding somebody reverse engineered by observation, not a derived rule.
2. `new_finger_id` is taken from the *count* of currently enrolled fingers. If slots are not allocated densely, for example after a delete, the new id can collide with an existing one. Check `finger_info` for every slot and pick a genuinely free id instead of using the count.

Record the exact bytes the chip returns after commit, including anything that arrives after the 2 bytes you expected, and put it in `findings.md`.

### `delete`

Payload is 2 bytes: finger id, 0.

### `delete_subsid`

Same 72 byte shape as commit. The payload is the sub id byte plus bytes 2 onward of that finger's `finger_info` response, padded to 69.

### `wipe_all`

No response, and no confirmation from the chip. Takes roughly 5 seconds. Poll `enrolled_num` afterwards to check. **Behind an explicit CLI flag only. Never in tests, never in cleanup.**

### `reset_device`

The table bytes are correct and complete. `0x40` followed by seven ASCII bytes: apostrophe (`0x27`) then `WDTRST`. The apostrophe is part of the literal, not a separator. Eight bytes total, no response.

Triggers a watchdog reset, so the device disconnects and re-enumerates. Your transport layer must handle the reconnect.

### `capture_start`

Reads `2 * width * height` bytes on `0x82`, 16 bit little endian pixels. Get the dimensions from `sensor_size` first. This is debug only. Matching still happens on-chip and this image is never used for authentication.

### `read_register`

64 registers, 0 to 63. Response byte 0 is the value, byte 1 is the status.

---

## Error codes

Status byte values. High nibble zero means no error.

| Code     | Meaning                          |
| -------- | -------------------------------- |
| `0x41` | Move slightly down               |
| `0x42` | Move slightly right              |
| `0x43` | Move slightly up                 |
| `0x44` | Move slightly left               |
| `0xdd` | Maximum enrolled fingers reached |
| `0xfb` | Sensor dirty or wet              |
| `0xfd` | Finger not enrolled              |
| `0xfe` | Finger area not enough           |

Mapping to fprintd D-Bus status strings in Phase 6:

- `0x41` to `0x44` map to `enroll-retry-center-finger` / `verify-retry-scan-too-short`. These are **retries**, not failures.
- `0xfb` and `0xfe` are retries too.
- `0xfd` during verify is `verify-no-match`.
- `0xdd` is a hard failure.

The high-nibble-zero rule is described in the source as eyeballed, not derived. Treat any unexpected status byte as an error and log it rather than assuming success.

---

## Enroll sequence

1. `enrolled_num`. Read the current count.
2. `verify` in a loop until it returns `0xfd` (not enrolled). This confirms the finger being enrolled is new. Any other result means the finger is already stored.
3. `enroll` eight times, once per touch, with the attempt counter incrementing. Handle retry codes without advancing the counter.
4. `check_enrolled_collision`. Abort if it reports a collision.
5. `commit` with the sub id and user data payload.
6. Status byte 0 means success.

On any exception or Ctrl-C at any point, send `abort` (`40 ff 02`) before releasing the interface. Without it the chip stays mid-session and the next command reads a stale response.

---

## DO NOT SEND

These opcodes appear in the source's notes. They are recorded here so nobody reuses the values by accident. **They are not part of this driver and must never be sent.**

| Opcode                            | What it does                        | Why not                                                                       |
| --------------------------------- | ----------------------------------- | ----------------------------------------------------------------------------- |
| `42 01 52 55 4E 49 41 50`       | Switch to bootloader                | Firmware update mode. Bricks the sensor if interrupted.                       |
| `40` + `(0x80 + reg)` + value | Write register                      | Writes device configuration. No documented safe values.                       |
| `40 ff 14 XX`                   | Set firmware sensor mode            | Changes WBF operating mode. Not needed.                                       |
| `00 10 00` / `00 10 01`       | Set EC pin state                    | Touches embedded controller signalling.                                       |
| `40 ff 0a` + 35 bytes           | Unclear, possibly ECC enroll commit | Meaning unknown. Unknown semantics plus flash write is the worst combination. |

---

## Differences from the 0c4c family

**Fill this in as you confirm each command.** This table is the novel output of the project and the thing worth publishing.

| Command | Expected (0c4c source) | Observed (0c90) | Notes |
| ------- | ---------------------- | --------------- | ----- |
| `fw_ver` | 2 bytes, major then minor | `01 08` | Matches. Agrees with `bcdDevice 0108`. |
| `sensor_size` | 4 bytes, width `b0+1`, height `b2+1` | `4f 00 4f 00`, 80 x 80 | Matches, off-by-one confirmed. |
| `enrolled_num` | byte 1 is the count | `40 00`, count 0 | Matches. Byte 0 echoes the command's `0x40`. |
| `finger_info` | 70 byte record per slot | **2 bytes, `40 ff`**, ids 0 to 9 | **Differs.** No 70 byte record seen. Meaning unresolved, see Q-002. |

---

## Open questions for 0c90

1. Do the endpoint addresses match? Confirm against `lsusb -v` before anything else.
2. Is `total_attempts` really 8, or does 0c90 report a different stage count? Check whether any response field carries it rather than hardcoding.
3. How many slots does this sensor have? Enroll until `0xdd` on a throwaway basis only if you are willing to wipe, otherwise probe `finger_info` across ids 0 to 9.
4. Does `commit` return the assigned template id anywhere, or does the host have to track it? The source assumes the host knows it.
5. What exactly does the chip send after `commit` on 0c90? This is the whole enroll bug.
