# elanprint-rs

A userspace driver for the ELAN `04f3:0c90` fingerprint sensor, written in pure
Rust, with no kernel module and no C dependencies.

The sensor is a match-on-chip device: an ARM Cortex-M4 with its own writable
flash, enumerating as a vendor-specific USB interface (class 255) with four
bidirectional bulk endpoint pairs and no kernel driver bound to it. Enrolment,
template storage and matching all run in the sensor firmware. The host drives it
with short vendor commands and reads back two and three byte status replies, so
there is no image processing anywhere in this codebase and no fingerprint image
ever crosses the bus.

libfprint carries no entry for this product id, so on a laptop that ships one
the reader enumerates and then sits idle for the life of the machine. This
driver claims interface 0 through usbfs, speaks the vendor protocol recorded in
`docs/protocol.md`, and serves the result on the D-Bus interface that fprintd
defines. Owning that bus name is what makes `pam_fprintd`, the lock screen and
GNOME greeter login work, with no kernel module and no edit to any file under
`/etc/pam.d`.

Templates are created, stored and compared inside the chip and never reach the
host, so the most the host ever learns from a touch is which slot id matched.

---

### Table of Contents

- <sub>[Status](#status)</sub>
- <sub>[Do I have this sensor?](#do-i-have-this-sensor)</sub>
- <sub>[Requirements](#requirements)</sub>
- <sub>[Install](#install)</sub>
  - <sub>[Templates outlive the uninstall](#templates-outlive-the-uninstall)</sub>
  - <sub>[Development mode](#development-mode)</sub>
- <sub>[Use](#use)</sub>
- <sub>[What this project found](#what-this-project-found)</sub>
- <sub>[Tech stack](#tech-stack)</sub>
- <sub>[Folder structure](#folder-structure)</sub>
- <sub>[Architecture](#architecture)</sub>
  - <sub>[Crate layering](#crate-layering)</sub>
  - <sub>[The login path](#the-login-path)</sub>
- <sub>[Safety](#safety)</sub>
- <sub>[Unverified elsewhere](#unverified-elsewhere)</sub>
- <sub>[Development](#development)</sub>
- <sub>[Credits](#credits)</sub>
- <sub>[License](#license)</sub>

---

## Status

It works. My laptop is also the only machine it has ever run on.

Ubuntu 24.04.4 LTS, kernel 7.0.0-31-generic, GNOME Shell 46.0, fprintd 1.94.3,
x86_64. Every byte in `docs/findings.md` came off that machine. I have not tried
a second laptop, a second sensor or another distribution.

| Works | Evidence |
| ----- | -------- |
| Enrol, 8 touches, writes flash | `commit` answered `40 00` in 103 ms |
| Verify a known finger | 10 of 10 matches, plus 5 of 5 in a later run |
| Reject an unknown finger | 10 of 10 no-match, zero retries |
| Two fingers in separate slots | left index matched id 0, right index id 1 |
| Template survives a reboot | count `40 01` and a match after a power cycle |
| Lock screen unlock | opened by the `gdm-fingerprint` PAM service |
| Greeter login after logout | `session opened for user ... (gdm-fingerprint)` |

Eight of the sixteen commands in `docs/protocol.md` are `confirmed`, meaning
they were sent on this device with the response recorded. The rest are
`documented`: the bytes are transcribed from a source but have never been run
here.

Not done: `delete` has never been run under the current code, `wipe_all` has
never been sent at all, and the 8 stage count is still borrowed from a
different chip rather than confirmed on this one.

---

## Do I have this sensor?

```bash
lsusb -d 04f3:0c90
```

One line back means yes:

```
Bus 001 Device 002: ID 04f3:0c90 Elan Microelectronics Corp. ELAN:ARM-M4
```

Nothing back means this driver is not for your machine. `04f3:0c90` is the only
supported device, and the other ELAN ids (`0c00`, `0c4c`, `0c5e`) speak a
related but different protocol. The differences found here are why sending 0c90
bytes to them is not safe, so the daemon refuses an ELAN sensor it does not know
rather than probing it.

Check your Ubuntu version too:

```bash
lsb_release -d      # 24.04 or newer
```

---

## Requirements

This is `04f3:0c90` only, and the sensor is the one requirement with no
flexibility: other ELAN product ids answer differently, and the protocol
differences recorded in `docs/findings.md` are why you cannot assume otherwise.

- Ubuntu 24.04 or newer. Older releases and other distributions are untested,
  and the installer warns rather than refuses.
- Linux with usbfs. `nusb` talks to `/dev/bus/usb` directly.
- systemd, for the service unit and the suspend and resume hooks.
- D-Bus system bus, to own `net.reactivated.Fprint`.
- `libpam-fprintd`, for login. This driver answers fprintd's D-Bus interface,
  so fprintd's own PAM module does the authentication.
- `getent` from `libc-bin`, for resolving account names through NSS. Present
  on any normal install.
- Rust 2021 edition. No minimum version is pinned yet.

No C dependencies: no libfprint, no glib and no `-sys` crates, so there is
nothing to build beyond `cargo build`.

---

## Install

```bash
cargo build --release -p elanprintd
sudo ./tools/install.sh
```

The installer checks before it touches anything: that the sensor is present
and is `04f3:0c90`, that this is Ubuntu 24.04 or newer, that systemd is
running, and that `/etc/pam.d/gdm-fingerprint` and `pam_fprintd.so` exist. It
refuses without the sensor and warns on the rest.

Then it installs the binary to `/usr/libexec`, creates an empty
`/var/lib/elanprint` for the slot mapping, installs the udev rule and the
systemd unit, masks the stock `fprintd.service` and starts the daemon.

The store starts empty and is never seeded. Enrolling a finger writes it.

`fprintd` is masked because it owns `net.reactivated.Fprint` and has no driver
for this sensor, so if it wins the name the greeter sees a reader with no
device.

Nothing under `/etc/pam.d` is touched. Ubuntu already ships
`/etc/pam.d/gdm-fingerprint` with `auth required pam_fprintd.so` and no
`@include common-auth`, so owning the bus name is the whole integration.

To undo all of it:

```bash
sudo ./tools/uninstall.sh
```

That removes the binary, the unit and the udev rule, and unmasks fprintd. The
store is kept. Password login is never affected either way.

### Templates outlive the uninstall

Uninstalling does not erase anything from the sensor. Templates live in the
chip's own flash, so removing the software leaves them there and they still
work.

Two things about this chip make that more serious than it sounds. The login path
sends the finger name `any`, so the chip matches against every template it
holds rather than only the ones belonging to the account logging in, and
`finger_info` answers the same bytes for an occupied slot and an empty one, so
nothing can enumerate what is still stored.

Put those together and a template enrolled by a previous owner keeps
authenticating after a reinstall, under a different account, with nothing on
the host recording that it exists. The uninstaller reports how many templates
remain and says so.

If you are passing this machine on, erase them:

```bash
sudo ./tools/uninstall.sh --wipe-device
```

That asks for typed confirmation and refuses to run without a terminal, so it
cannot happen from a script. It sends `wipe_all`, which is documented but has
never been run on my hardware, then reads the count back to check.

`--delete-store` removes the host mapping too. It warns when templates remain,
because deleting the mapping is what orphans them.

### Development mode

Runs on the session bus with a throwaway store, so the installed daemon and
`/var/lib/elanprint` are untouched. No root.

```bash
./tools/dev.sh status      # sensor, service, bus owner, dev store
./tools/dev.sh up          # daemon and app together
./tools/dev.sh daemon      # daemon only, debug logging
./tools/dev.sh cli info    # elanprint-cli against the dev setup
./tools/dev.sh clean       # delete the dev store
```

The sensor is the one thing dev mode cannot share: whichever process claims
USB interface 0 keeps it. Stop the service first, and `dev.sh` says so rather
than failing with "Device or resource busy".

```bash
sudo systemctl stop elanprintd
```

---

## Use

Enrol and delete through any fprintd client, or through the demo app. The CLI
is for reading state and for debugging.

The installer only puts the daemon on the system, so the CLI runs from the
build tree:

```bash
cargo build -p elanprint-cli
./target/debug/elanprint-cli info          # firmware, sensor size, count. read only
./target/debug/elanprint-cli probe         # open, list endpoints, release. sends nothing
./target/debug/elanprint-cli verify        # wait for a touch, report the match
./target/debug/elanprint-cli slots --prime # read every slot record
./target/debug/elanprint-cli sync --store PATH   # host mapping against the chip
```

`enroll` writes flash and `enroll-plan` prints the bytes it would send without
sending them. Both exist for bring-up, not for daily use.

The daemon holds USB interface 0 for its lifetime once claimed, so the CLI
reports "Device or resource busy" while the service is running. Stop the
service to use the CLI.

---

## What this project found

The useful output is not the code, it is `docs/protocol.md` and
`docs/findings.md`. Four behaviours of this sensor are recorded there and, as
far as we know, nowhere else.

**The arming rule.** `verify` (`40 ff 03`) only answers if `enrolled_num`
(`40 ff 04`) was sent first on the same claimed interface. Without it the chip
accepts the command and then stays silent forever, on every endpoint. That was
confirmed over three silent runs and one successful one, and no public source
mentions it.

**`finger_info` cannot find an occupied slot.** The 0c4c family returns a 70
byte record per slot, but on 0c90 every slot id answers with 2 bytes, `40 ff`,
whether it holds a template or not. A host cannot scan the chip to learn what is
stored, which leaves `enrolled_num` as the only reading that proves anything.
Getting this wrong destroys templates, because an enrolment that trusts a slot
scan will overwrite a live one.

**`verify` carries no finger id.** It is three bytes with no payload. The chip
matches against every template it holds and returns whichever id hit, so asking
to verify one specific finger is not something the protocol supports, and a
caller that names a finger has to compare the returned id itself.

**The three IN endpoints are not interchangeable.** `0x82` is image, `0x83` is
status and `0x84` is touch-wait. Posting a read on the wrong one does not fail,
it hangs until the timeout. The collision check replies on `0x83` while the
enrol samples either side of it reply on `0x84`, which is subtle enough that it
cost me two failed enrolments before I found it.

---

## Tech stack

| Layer | |
| ----- | - |
| Language | ![Rust](https://img.shields.io/badge/Rust-2021-000000?logo=rust&logoColor=white) |
| USB transport | ![nusb](https://img.shields.io/badge/nusb-0.1-dea584) ![usbfs](https://img.shields.io/badge/usbfs-kernel-555) |
| Async | ![tokio](https://img.shields.io/badge/tokio-1-000000?logo=rust&logoColor=white) |
| IPC | ![zbus](https://img.shields.io/badge/zbus-4-1793d1) ![D-Bus](https://img.shields.io/badge/D--Bus-system%20bus-4a90d9) |
| Desktop app | ![eframe](https://img.shields.io/badge/eframe-0.32-7c3aed) ![egui](https://img.shields.io/badge/egui-0.32-7c3aed) |
| Service | ![systemd](https://img.shields.io/badge/systemd-unit-30b6ff?logo=systemd&logoColor=white) ![udev](https://img.shields.io/badge/udev-uaccess-555) |
| Auth | ![PAM](https://img.shields.io/badge/PAM-pam__fprintd-e8574a) ![fprintd](https://img.shields.io/badge/fprintd-1.94-e8574a) |

No C dependencies anywhere in that list. `nusb` speaks usbfs directly, so
there is no libusb, no libfprint and no glib to build against.

---

## Folder structure

```
elanprint-rs/
├── crates/
│   ├── elanprint-usb/      transport: open, claim, bulk transfers, timeouts, cancellation
│   ├── elanprint-proto/    pure protocol: command encoding, response parsing, enrol state machine
│   ├── elanprint-store/    user and finger name to on-chip slot, and slot policy
│   ├── elanprint-algo/     login decision engine, session policy and status mapping
│   ├── elanprintd/         system daemon, owns net.reactivated.Fprint on the system bus
│   ├── elanprint-cli/      developer tool for bring-up and debugging
│   └── elanprint-login/    desktop app for enrolment and self-test, not an auth surface
├── docs/
│   ├── protocol.md         the command table; nothing reaches the device unless it is here
│   └── findings.md         what the device actually did, append only
├── tools/
│   ├── install.sh          install the daemon, the udev rule and the systemd unit
│   ├── uninstall.sh        undo the install, keeping the store by default
│   ├── dev.sh              run on the session bus with a throwaway store, no root
│   └── check.sh            build, test and clippy across the workspace
└── LICENSE
```

Only `elanprint-usb`, `elanprintd` and `elanprint-cli` need real hardware, and
`elanprintd` reaches it through a trait it can be tested against a fake for.
Everything else builds and tests with no sensor attached.

---

## Architecture

Three processes can drive the sensor and only one may hold it at a time:
whoever claims USB interface 0 keeps it until the process exits. In normal
operation that is `elanprintd`, running as root and owning
`net.reactivated.Fprint` on the system bus, so the login path and the GUI both
go through a single arbiter instead of racing for the device.

The diagram below is grouped by privilege boundary. A solid arrow is the normal
path, a dashed arrow is conditional.

<p align="center">
  <img src="assets/architecture-system.png" alt="elanprint-rs system architecture: session, login path, system service, kernel and device" width="940">
</p>

The store holds no biometric data. Matching happens on the sensor, so the host
only ever learns that a given slot matched. The mapping file exists to turn
that id back into a finger name for a user, and to keep enrolment from reusing
a slot that is already in use.

### Crate layering

Crates are split by what they need to run. An arrow means depends on, it points
downward only, and `elanprint-proto` is never allowed to reach the transport.
That rule is what lets the entire protocol be tested with no hardware attached.

<p align="center">
  <img src="assets/architecture-crates.png" alt="Crate layering: binaries, pure crates and the single hardware-facing crate" width="900">
</p>

`elanprintd` is generic over a `Transport` trait. Tests drive it against a fake
that records every byte, which is how the erase paths are proven with no sensor
present: a test asserts that no `delete`, `delete_subsid` or `wipe_all` opcode
ever reaches the wire.

| Crate | Does | Needs hardware |
| ----- | ---- | -------------- |
| `elanprint-usb` | moves bytes, picks no endpoints, knows no opcodes | yes |
| `elanprint-proto` | command encoding, response parsing, enrol state machine | no |
| `elanprint-store` | user and finger name to on-chip slot, slot policy | no |
| `elanprint-algo` | login decision engine | no |
| `elanprintd` | the daemon, owns `net.reactivated.Fprint` | through a trait |
| `elanprint-cli` | debug and bring-up tool | yes |
| `elanprint-login` | the desktop app | no |

### The login path

This is the sequence that matters, because two of the findings above are load
bearing in it: the session must be armed before any touch-wait command, and the
id the chip returns must be checked against the slots the claiming user owns.

<p align="center">
  <img src="assets/architecture-login.png" alt="Fingerprint login sequence from the greeter to the sensor and back" width="940">
</p>

That slot check is what the security of the login path rests on. `verify`
carries no finger id, so without it any enrolled template authenticates any
account, including one a previous owner left in flash.

The workspace has 114 tests and none of them need the device.

---

## Safety

This sensor is an ARM Cortex-M4 with writable flash, soldered to the board.
There is no reflash path, so a wrong command can leave the hardware unusable
with no way to recover it in software. The rules that follow from that:

- Nothing is sent unless it appears in `docs/protocol.md`.
- Opcodes are never looped over or guessed, because walking a range of opcode
  values on a flash-capable MCU is fuzzing it.
- `delete`, `delete_subsid` and `wipe_all` are erase operations with no undo.
  In this codebase erase is reachable from exactly one function, and only with
  a value that can only be built from an explicit user request. No comparison
  between the host mapping and the chip can reach it.
- The host mapping is never authoritative over chip flash. Enrolment is
  floored at `enrolled_num`, so a host file that disagrees with the device can
  never hand out a slot the device says is in use.
- Any error sends `abort` (`40 ff 02`) before the interface is released.
  Without it the chip stays mid-session and the next command reads stale.

---

## Unverified elsewhere

Everything below works here and has never been observed anywhere else. If you
run this on another machine, these are the parts most likely to differ, and
`docs/findings.md` takes appended results.

**Firmware.** One unit, reporting 1.8. The daemon logs the version on the first
claim, so a difference is visible in `journalctl -u elanprintd`. Two of the four
findings above may be revision specific:

- the arming rule. If a touch-wait times out, the error now says the arming
  rule may not hold on that firmware rather than just "timeout".
- `0xff` from `finger_info` meaning an empty slot. On 1.8 it means empty, not
  the stuck sensor the 0c4c source describes.

**The 8 stage count** is borrowed from 0c4c. Nothing on this chip reports a
stage count, so it is a guess that happens to work here. Override it:

```bash
sudo systemctl edit elanprintd     # Environment=ELANPRINT_ENROLL_STAGES=10
```

The accepted range is 1 to 32 and anything else falls back to 8. The value in
use is logged at startup and again on every enrol, alongside the firmware
version, so a wrong guess is diagnosable rather than a silent hang.

**`wipe_all` has never been run.** `--wipe-device` implements it from the
documented bytes and checks the count afterwards, but I have never watched it
run, so treat the first run as a test.

**Other hardware.** A second 0c90 unit, a second firmware revision and every
non-Ubuntu distribution are all untested. So is `delete` under the current
code, and `wipe_all` has never been sent at all.

**Desktops other than GNOME.** Login works here because Ubuntu ships
`/etc/pam.d/gdm-fingerprint`. KDE, Cinnamon and XFCE do not have that file, so
enrol and verify would work but greeter login would need PAM configuration this
project deliberately does not touch.

---

## Development

```bash
./tools/check.sh    # build, test and clippy across the workspace
```

Clippy runs with `-D warnings`, and `unwrap` and `expect` are denied outside
tests.

The architecture images are rendered from the mermaid sources beside them in
`assets/`, because GitHub does not always render mermaid in a README. After
editing one, regenerate it with:

```bash
npx @mermaid-js/mermaid-cli -i assets/architecture-system.mmd \
  -o assets/architecture-system.png -b white -w 1500 -s 2
```

`docs/findings.md` is append-only and is the record of what the device actually
did. When a response does not match the documented layout, the device is right
and the document is wrong, so record the difference rather than bending the
parser to fit.

---

## Credits

Protocol groundwork from [`depau/elanpoc`](https://github.com/depau/elanpoc)
by Davide Depau, MIT licensed, a proof of concept for the ELAN 0c4c family.
It does not cover 0c90.

`docs/protocol.md` is the part that owes it a debt. The opcode table, the
`commit` sub id formula and the shape of the enrol sequence were transcribed
from that project and are marked `documented` until run here. Everything in
the differences table is the gap between that family and this sensor,
measured on this hardware.

No source code from elanpoc is present. It is a Python proof of concept and
every line of Rust in `crates/` was written for this project.

---

## License

MIT. ([LICENSE](LICENSE))

The protocol documentation in `docs/protocol.md` derives from `depau/elanpoc`,
which is also MIT licensed, and keeps its attribution.
