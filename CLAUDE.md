# CLAUDE.md

`elanprint-rs` is a userspace fingerprint driver for the ELAN 04f3:0c90 sensor.
Pure Rust, no kernel module. Ubuntu 24.04. It talks to real hardware that can be
destroyed by bad bytes, and it sits on the login path of the machine it runs on.

It works: enrolment from the app, verification, and GDM greeter login through
`pam_fprintd` and the system daemon. Treat this as shipped software, not as a
project being explored.

---

## Layout

```
crates/
  elanprint-usb      transport. open, claim, bulk transfers, timeouts, cancellation
  elanprint-proto    pure encode and parse. bytes in, bytes out, no I/O
  elanprint-algo     session policy, slot selection, status mapping
  elanprint-store    user and finger name to on-chip slot, /var/lib/elanprint
  elanprintd         system daemon, owns net.reactivated.Fprint
  elanprint-cli      developer tool
  elanprint-login    enrolment UI. not an auth surface
docs/protocol.md   the command table. nothing reaches the device unless it is here
docs/findings.md   observed device behaviour, append only
tools/             dev and install scripts
systemd/ udev/ polkit/ dbus/   install artefacts
```

Dependency direction is one way: `elanprint-proto` never depends on `elanprint-usb`.
The CLI and the daemon wire transport to protocol. If a protocol function cannot
be unit tested without hardware attached, it is in the wrong crate.

---

## 1. Device safety

The only section where a mistake is unrecoverable. Everything else is style.

The sensor is an ARM Cortex-M4 with writable flash, soldered to the board. There
is no reflash path. A wrong command ends the project and costs a hardware repair.

| Never                                                                              | Why                                                     |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------- |
| Send bytes not present in`docs/protocol.md`                                      | Unknown opcodes can hit flash write or bootloader entry |
| Add a command to`docs/protocol.md` yourself                                      | The table is sourced and confirmed, not derived         |
| Send anything from the DO NOT SEND table                                           | Bootloader entry, register writes, EC pin control       |
| Loop over opcode values                                                            | That is fuzzing a flash-capable MCU                     |
| Retry a failed command with modified bytes                                         | A wrong guess twice is still a wrong guess              |
| Call`wipe_all`, `delete` or `delete_subsid` without an explicit user request | No undo. This erased a live template once already       |
| Call any destructive command in a test or in cleanup                               | Tests run unattended                                    |
| Leave a session open after an error                                                | Send`abort` (`40 ff 02`) before releasing           |

If a task needs a command that is not in the table: stop, report the gap, wait.
Do not work around it.

**The host store is never authoritative over device flash.** A disagreement
between `/var/lib/elanprint/prints.json` and the chip is resolved by trusting the
chip. Deleting a template requires an explicit user delete action and nothing
else. A sync that erased flash to match an empty store is how a working template
was lost.

---

## 2. Protocol facts

Confirmed on this hardware, and not in any public source. Code that contradicts
them is wrong.

- **Arming.** A touch-wait command gets no reply unless `enrolled_num`
  (`40 ff 04`) precedes it on the same claim. Without it, `verify` and `enroll`
  sit silent indefinitely. Hold the claim across the whole sequence.
- **Endpoints.** `0x01` OUT for commands. `0x82` image, `0x83` status, `0x84`
  touch-wait. Never collapse the three IN endpoints. Never write to `0x02`,
  `0x03` or `0x04`; they are OUT endpoints that reach firmware.
- **`0xff` from `finger_info` means empty slot** on 0c90, not the stuck sensor
  the 0c4c source documents. The documented remedy was applied and changed
  nothing.
- **`verify` carries no finger id.** The chip matches against every template and
  returns whichever hits. Reporting that raw as a match for a named finger is a
  false accept. Resolve the name to a slot and compare.
- **`abort` returns nothing** despite `in_len: 2`. Do not wait for a reply.
- **`finger_info` returns 2 bytes for an empty slot**, not 70. Short reads keep
  their bytes.
- **`enrolled_num` byte 1 is a status byte.** Status 0 and count 0 both read
  `00`. Do not treat it as a bare count.
- **`total_attempts = 8` came from 0c4c.** Nothing on this chip reports a stage
  count. It is host-supplied, defined once, shared between proto and the daemon.

`docs/findings.md` is append only. When a device response disagrees with the
document, the device is right. Record the difference. Never bend a parser to fit.

---

## 3. Rust

| Rule                                                              | Limit                                                                  |
| ----------------------------------------------------------------- | ---------------------------------------------------------------------- |
| No C dependencies                                                 | `nusb` not `rusb`. No `pam-sys`, no `glib`, no `-sys` crates |
| No`unwrap()` or `expect()` outside tests                      | Enforced by workspace lint                                             |
| One`thiserror` enum per crate                                   | `#[from]` upward. No `Box<dyn Error>` in libraries                 |
| `anyhow` in binaries only                                       | `elanprint-cli`, `elanprintd`, `elanprint-login`                       |
| Every USB read has an explicit timeout                            | Per command. Never a global default                                    |
| Every wait is cancellable                                         | `tokio::select!` against a `CancellationToken`                     |
| Every spawned operation emits a terminal status on its error path | A silent failure leaves the UI waiting forever. This was a real bug    |
| No`#[allow(...)]` without a one-line reason above it            |                                                                        |

Before handing work back: `cargo build`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`.

---

## 4. Daemon

- Owns `net.reactivated.Fprint` on the **system** bus. `fprintd.service` must be
  masked or it races for the name.
- Access control reads the caller uid from the bus via `GetConnectionUnixUser`,
  never from the message body. Allow uid 0 or the owner of the prints; refuse
  everything else with a real D-Bus error, never a silent no-op.
- D-Bus status strings come from the live fprintd 1.94 introspection in `docs/`.
  `pam_fprintd` string-matches them. A typo means enrolment hangs with no error.
- One consumer of the device at a time. Two concurrent operations desync the
  protocol and produce symptoms that look like firmware bugs.
- The daemon holds USB interface 0 for its lifetime, so `elanprint-cli` and
  `tools/run.sh` fail with "Device or resource busy" until the service stops.
  Expected, not a regression.

---

## 5. Login path

GDM uses `/etc/pam.d/gdm-fingerprint`, which ships with `auth required pam_fprintd.so` and does not include `common-auth`. Owning the bus name is the
whole integration. **No file under `/etc/pam.d` is edited or needs to be.**

`sudo` by fingerprint would require `common-auth`, the one change that can lock
the user out. Out of scope. Do not propose it as a fix for anything.

`tools/uninstall-system.sh` reverts the install completely. Password login always
works regardless.

---

## 6. Working rules

One task at a time. Report after each. Do not chain work.

**Stop and ask** for: a command not in `docs/protocol.md`, anything writing
device flash outside an explicit request, anything under `/etc/pam.d`,
`systemctl mask` or `unmask` (name the unit first), installing a udev rule (show
it first), adding a dependency, or `sudo` beyond `udevadm`, `tee` to
`/etc/udev/rules.d/70-elanprint.rules`, and `modprobe usbmon`.

**Never commit.** Stage nothing, tag nothing, amend nothing. The user commits.
Leave the tree clean and say what changed.

**Touch tests need the user physically present.** Start the run, say so, wait for
their message confirming the touch, and only then read the result. A run without
that confirmation is undetermined and is not evidence either way.

Capture with usbmon when a device interaction matters. `tools/usbmon_filter.py`
decodes the pcapng; tshark is not installed.

---

## 7. System resources

This runs on the user's daily laptop. A frozen desktop costs them their open work.

Measure, do not assume:

```bash
free -m | awk '/Mem:/ {print "free:", $7"MB"}'; uptime | grep -o 'load average.*'; nproc
```

Reserve 3 GB for the desktop. One heavy process at a time. Never `cargo build -j`
with all threads; leave 2 cores free. If the budget is below floor, refuse and say
so rather than starting a job that will swap.

Worker agents: only one may build or touch the device at a time. Workers produce
files. They never commit and never touch the device.

---

## 8. Comments

Default is no comment. Write one only if removing it would cause someone to break
the code. Max 10 words, one line.

Never restate a name. No banners, no phase or step numbers, no commented-out
code, no comments on code you did not change. `TODO` and `FIXME` need a tracking
reference. More than three comments in a file means the code is unclear; rewrite
it instead.

One exception, a magic byte value names its command:

```rust
const CMD_ABORT: &[u8] = &[0x40, 0xff, 0x02];  // abort, per protocol.md
```

---

## 9. Commit messages

You do not commit, but you write the message when asked.

```
type(scope): short subject in plain English

- what changed
- what changed
```

Types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `ci`, `perf`.

Scopes: `usb`, `proto`, `algo`, `store`, `daemon`, `dbus`, `login`, `cli`,
`udev`, `polkit`, `systemd`, `docs`, `deps`, `ci`.

Subject: imperative, lowercase, no full stop, under 60 characters. Say what
changed, not what you did. Body: bullets only, max 5, skip it if the subject says
everything.

```
fix(usb): read touch-wait replies on the correct endpoint

- verify and enroll reply on 0x84, not 0x83
- was a timeout on every enroll sample
```

---

## 10. Writing

Plain, short English. Same rules for docs and for replies.

No em dashes; use commas, periods, colons or parentheses. No buzzwords, no
filler, no "comprehensive", "robust", "seamless", "leverage". Short sentences.

State findings as facts with evidence. "Chip returned `40 fd` on `0x84` after
20.87s" beats "verification is working well". Never invent a number. If you did
not observe it, do not write it.

---

## 11. Anti-patterns

| Never                                            | Why                                                |
| ------------------------------------------------ | -------------------------------------------------- |
| Guess a byte value                               | Section 1. This is the one that ends the project   |
| Bend a parser to match the documentation         | The device is the truth. Record the difference     |
| Let the host store drive a device delete         | This erased a live template once                   |
| Report a raw match as a match for a named finger | False accept. Resolve the name to a slot first     |
| Swallow an error in a spawned task               | The UI waits forever with no signal                |
| Collapse the three IN endpoints                  | They are different on purpose                      |
| Hardcode a stage count in the UI                 | It comes from`num-enroll-stages`                 |
| Write image processing code                      | The chip matches on-chip.`capture` is debug only |
| Treat the enrolment app as an auth surface       | Real auth is PAM. The app unlocks nothing          |
| Edit`/etc/pam.d` to fix something              | The login path needs no PAM edit                   |
| Commit, stage or tag                             | The user does that                                 |
| Report success when a workaround was needed      | If it needs a cancel to save, it is broken. Say so |
