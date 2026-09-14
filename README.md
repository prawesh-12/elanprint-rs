# elanmoc-rs

Userspace fingerprint driver for the ELAN 04f3:0c90 sensor. Pure Rust, no C
dependencies. Match-on-chip: templates never leave the sensor.

## Layout

- `crates/elanmoc-usb`: transport, moves bytes, no protocol knowledge.
- `crates/elanmoc-proto`: command codecs, response parsing, enroll machine.
- `crates/elanmoc-store`: unix user to on-chip slot mapping.
- `crates/elanmoc-cli`: `elanmoc-cli` debug and admin tool.
- `crates/elanmocd`: `elanmocd` daemon, owns `net.reactivated.Fprint`.
- `crates/elanmoc-algo`: login decision engine, pure, no I/O.
- `crates/elanmoc-login`: `elanmoc-login` desktop app, demo front end.
- `docs/protocol.md`: every byte the driver may send. Source of truth.
- `docs/findings.md`: observed device behaviour, append-only.

## Run everything

One command runs the backend daemon and the frontend app together:

```bash
./tools/run.sh
```

Both use the session bus (`ELANMOC_BUS=session`), so no root and no fight
with fprintd over the system name. The store lands in
`/tmp/elanmoc-prints.json` via `ELANMOC_STORE`. The system bus stays
untouched until GATE 6.

Single commands per binary:

```bash
cargo login-app                  # desktop login window
cargo run -p elanmoc-cli -- info # sensor status, read only
```

`elanmocd` on the system bus needs root for the bus name. That setup waits
for GATE 6.

## Run the login app

The app is a demo front end over the daemon. Granting unlocks the window
only. Real session auth stays on the PAM path.

```bash
cargo login-app
```

`ELANMOC_BUS=session` points it at a dev daemon instead of the system bus.

Flow in the window: Connect, pick user and finger, Login with fingerprint,
touch when asked. Cancel stops the wait. Sign out resets the screen.

The app needs a reader on the system bus. Until `elanmocd` owns
`net.reactivated.Fprint` (GATE 6), it reports "no reader" and nothing else
works. The daemon needs root to own the bus name:

```bash
sudo ./target/debug/elanmocd
```

`ELANMOC_STORE` points the daemon at a store file, default
`/var/lib/elanmoc/prints.json`. For unprivileged runs:

```bash
ELANMOC_STORE=/tmp/prints.json ./target/debug/elanmocd
```

## PAM rollback

Enabling fingerprint auth edits `/etc/pam.d/common-auth` through
`pam-auth-update`. If login breaks, a kept root shell reverts it:

```bash
sudo cp /etc/pam.d/common-auth.bak /etc/pam.d/common-auth
```

The backup is taken before anything is changed, in Phase 7, behind GATE 7.
Password fallback (`pam_unix.so`) stays reachable at all times.
