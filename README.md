# elanmoc-rs

Userspace fingerprint driver for the ELAN 04f3:0c90 sensor. Pure Rust, no C
dependencies. Match-on-chip: templates never leave the sensor.

## Layout

- `crates/elanmoc-usb`: transport, moves bytes, no protocol knowledge.
- `crates/elanmoc-proto`: command codecs, response parsing, enroll machine.
- `crates/elanmoc-store`: unix user to on-chip slot mapping.
- `crates/elanmoc-cli`: `elanmoc-cli` debug and admin tool.
- `crates/elanmocd`: `elanmocd` daemon, owns `net.reactivated.Fprint`.
- `docs/protocol.md`: every byte the driver may send. Source of truth.
- `docs/findings.md`: observed device behaviour, append-only.

## PAM rollback

Enabling fingerprint auth edits `/etc/pam.d/common-auth` through
`pam-auth-update`. If login breaks, a kept root shell reverts it:

```bash
sudo cp /etc/pam.d/common-auth.bak /etc/pam.d/common-auth
```

The backup is taken before anything is changed, in Phase 7, behind GATE 7.
Password fallback (`pam_unix.so`) stays reachable at all times.
