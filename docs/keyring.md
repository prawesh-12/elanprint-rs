# the login keyring

How fingerprint login leaves the GNOME keyring locked, why that is not a fault
in this driver, and what `elanprint-rs` does about it.

---

## The symptom

You log in with your finger. The desktop appears. Some minutes later, the first
time anything wants a stored secret, a dialog appears:

> The login keyring did not get unlocked when you logged into your computer.

Cancel it and the machine works fine, except saved Wi-Fi passwords, browser
logins and online account tokens are unavailable for that session.

From the journal on such a login:

```
gdm-fingerprint: gkr-pam: no password is available for user
gdm-fingerprint: pam_unix(gdm-fingerprint:session): session opened for user
gdm-fingerprint: gkr-pam: couldn't unlock the login keyring
```

The login succeeded. The keyring did not open, and the reason is in the first
line: nothing gave it a password.

---

## Why it happens

### What the keyring is

`gnome-keyring` stores secrets in `~/.local/share/keyrings/login.keyring`. That
file is encrypted. The key is derived from a password, and on a stock Ubuntu
install that password is your account password. The two are kept in step by
`pam_gnome_keyring`, which is why changing your account password normally
changes the keyring's too.

### How it normally unlocks

PAM carries the password you typed in an item called `PAM_AUTHTOK`, number 6 in
`security/_pam_types.h`. A password module sets it during authentication, and
`pam_gnome_keyring` reads it later in the same stack.

That is the whole mechanism. `gnome-keyring` has one unlock path and it starts
with a typed password.

### Why a fingerprint cannot use it

A fingerprint is not a password and cannot be turned into one.

On this sensor, matching happens on the chip. The host sends `40 ff 03` and gets
two bytes back, `40 00`, meaning slot 0 matched. There is no key material in
that answer, and nothing in a finger from which to derive one. The sensor does
not know your password and never did.

So `pam_fprintd` sets no authtok. `pam_gnome_keyring` runs, finds nothing, and
reports `no password is available for user`.

### The stack, as Ubuntu ships it

`/etc/pam.d/gdm-fingerprint`:

```
auth  requisite  pam_nologin.so
auth  required   pam_succeed_if.so user != root quiet_success
auth  required   pam_fprintd.so
auth  optional   pam_gnome_keyring.so
...
session optional pam_gnome_keyring.so auto_start
```

The keyring module is **already there** and already runs on every fingerprint
login. Nothing is missing from the wiring. What is missing is a value in
`PAM_AUTHTOK` for it to read.

---

## Why this is not a driver bug

Every fingerprint reader on Linux behaves this way, including the ones libfprint
has supported for years. Swap this sensor for a supported one and the dialog is
identical, because the gap is between `pam_fprintd` and `gnome-keyring`, and
neither is part of this project.

macOS and Windows solve it with hardware. Touch ID and Windows Hello keep the
password, or a key equivalent to it, wrapped inside a security chip and release
it when the biometric matches. The fingerprint does not become the password. It
opens a safe that holds one. Both halves had to be built deliberately: the
biometric side and the secret store side.

GNOME has no equivalent path. The pieces exist separately on Linux, and
`systemd-cryptenroll` already unlocks disks from a TPM or a FIDO2 token, but
nobody connected them to `gnome-keyring`. There is no upstream fix to wait for.

---

## The fix

### Shape

Keep a random secret sealed in the TPM. Unseal it during authentication, put it
in `PAM_AUTHTOK`, and let the keyring module that already runs do the rest.

The keyring is re-keyed to that secret, so the account password no longer opens
it and is never stored anywhere. If the sealed secret leaked, an attacker would
have the saved secrets, not your account password, not `sudo`, not any password
you reused elsewhere.

```
finger matches
  -> pam_fprintd returns success
  -> pam_elanprint_keyring unseals the secret, sets PAM_AUTHTOK
  -> pam_gnome_keyring reads it and unlocks the keyring
```

### Why the TPM

The secret has to survive a reboot and be readable by root at login without
anyone typing anything. On disk in plaintext it is a file anyone with the disk
can read. Sealed to the TPM it is bound to this machine's hardware, and
`systemd-creds` does the sealing and unsealing as a subprocess, so there is no C
dependency here, the same way the daemon shells out to `getent`.

### Why PCR binding is off

`systemd-creds encrypt` defaults to **PCR 7** when `--tpm2-pcrs=` is not given.
Verified in the man page on systemd 255.4. This project passes the flag
explicitly and empty, so nothing is bound.

PCR 7 tracks Secure Boot state. Two problems:

- Ubuntu ships dbx updates through `fwupd`, and each one moves PCR 7. The seal
  then stops opening on a machine that worked yesterday.
- It does not buy much. Any Microsoft-signed bootloader reproduces the same
  PCR 7, so a signed live image defeats it.

The trade is a real support cost for weak protection. The recovery key is the
safety net instead.

On a machine whose root filesystem is not encrypted, sealing is what stops the
blob being usable off the disk. That is not nothing, but it binds to the
machine, not to the OS running on it.

### Where the pieces live

| Piece | What it does | Runs as |
| ----- | ------------ | ------- |
| `pam_elanprint_keyring.so` | unseals, sets `PAM_AUTHTOK`, returns `PAM_IGNORE` | inside the PAM stack, root |
| `elanprint-keyring` | the setup tool, both halves | you, and root through pkexec |
| Keyring tab in the app | the same setup, with buttons | you |
| `elanprint-keyring-setup` | the library all of the above share | n/a |

### The module, in detail

It sits on the login path, so it is written to be incapable of blocking a login.

- **Always returns `PAM_IGNORE`.** A missing credential, a TPM that will not
  unseal, a hung helper, a panic: every path returns the same value, leaving
  exactly the pre-existing behaviour of a locked keyring and a working login.
- **`catch_unwind` around the whole entry point.** Unwinding across FFI into
  libpam is undefined behaviour and can abort `gdm-session-worker`. This only
  works while the crate is built with `panic = "unwind"`; setting
  `panic = "abort"` in the release profile would silently remove the guard.
- **A hard two second timeout on the helper**, then it is killed. `optional`
  protects against a bad return code, not against a hang, and a hang would
  freeze the greeter.
- **The secret is zeroized after use.** Linux-PAM copies it: in
  `libpam/pam_item.c` the `PAM_AUTHTOK` case calls `TRY_SET`, which is
  `_pam_strdup`.
- **Failures go to syslog** as `authpriv.err`, tagged `pam_elanprint_keyring`.
  The secret is never logged. Logging is compiled out of tests, so a test run
  cannot leave lines in the journal that read like a real failure.
- **In `gdm-password` it deliberately overwrites the authtok `pam_unix` set.**
  After the re-key the account password is the wrong key, so the value left
  there must be replaced. That line is commented in the source, because the
  instinct to preserve an existing authtok is wrong here.

---

## Using it

Everything below exists in the app's Keyring tab and in the CLI. They call the
same library.

### Turn it on

Open the app, Keyring tab, **Set up**. Or:

```bash
elanprint-keyring enable
elanprint-keyring enable --fingerprint-only   # leave password logins alone
```

In order: preflight, generate 32 bytes from `/dev/urandom` as hex, seal them,
**verify the seal round trips before anything else is touched**, write the
recovery key, back up and edit the PAM stacks, then re-key the keyring.

The order matters. If the TPM fails, nothing has changed. If the re-key fails,
PAM is wired but the keyring stays locked at login, which is where you started.

### Show the key

**Show key** in the app, or:

```bash
elanprint-keyring show-key
```

### Replace the key

**Replace key**, or:

```bash
elanprint-keyring rotate
```

Unseal the current key, generate and seal a new one, keep the old blob at
`keyring.cred.prev`, then re-key the keyring from old to new. No password is
asked for, because the keyring's current password is the old key. If the re-key
fails the old blob is restored, so a failed rotation leaves a working setup.

### Also unlock on password logins

Setup does both stacks unless you say otherwise. To add it later:

```bash
elanprint-keyring add-password-login
```

Without it, logging in with your password leaves the keyring locked, because
nothing supplies the secret on that path.

### Turn it off

**Turn off**, or:

```bash
elanprint-keyring disable
```

This removes the PAM lines. **It does not re-key the keyring**, which stays on
the TPM secret, so logins will prompt for it. To go back to your account
password, change it in Passwords and Keys: old is the recovery key, new is your
account password.

### State

```bash
elanprint-keyring status
```

Reports `cannot tell without root` for the sealed secret when run as a normal
user, because the directory is `0700`. That is deliberate. An earlier version
reported `no`, which would have led someone to seal a second secret over a
working one and strand their keyring.

---

## Files

| Path | Mode | What |
| ---- | ---- | ---- |
| `/var/lib/elanprint-rs/keyring.cred` | `0600 root` | the sealed secret |
| `/var/lib/elanprint-rs/keyring.cred.prev` | `0600 root` | the previous one, kept by a rotation |
| `/root/keyring-key.txt` | `0600 root` | the key in plaintext |
| `/usr/lib/<multiarch>/security/pam_elanprint_keyring.so` | `0644 root` | the module |
| `/root/gdm-fingerprint.bak.<epoch>` | as the original | PAM backup, one per edit |

### The plaintext copy

Setup writes the key to `/root/keyring-key.txt` and rotation replaces it, so the
key is not lost when someone skips writing it down.

**It is plaintext.** On a machine whose root filesystem is not encrypted, anyone
who can read the disk can read that file, which defeats the point of sealing the
secret. Delete it once the key is somewhere safe:

```bash
elanprint-keyring forget-recovery-file
```

The app offers the same on the Show key screen, and only while the file exists.

---

## What happens when something fails

| Failure | Result |
| ------- | ------ |
| TPM will not unseal | keyring stays locked, login works, dialog appears |
| Credential missing | same |
| `systemd-creds` hangs | killed after 2s, same |
| Module panics | caught, same |
| Module removed while the keyring is re-keyed | keyring prompts for the recovery key |
| PAM file edited wrongly | the line is `optional`, so login still works |
| Rotation interrupted after sealing | old blob restored from `keyring.cred.prev` |

The worst case is the behaviour you had before any of this existed.

---

## Recovery

**A failed unseal locks the keyring on every path at once.** After the re-key,
neither your account password nor your fingerprint opens it. Only the secret
does. That is the cost of the design, and the reason the recovery key exists.

What stops the TPM unsealing: clearing the TPM in firmware setup, moving the
disk to another machine, and, if PCR binding is ever turned on, a firmware or
Secure Boot change.

If it happens:

1. **You can still log in.** The module returns `PAM_IGNORE`, so login works and
   the keyring is locked. That is the pre-existing behaviour, not a lockout.
2. **Type the recovery key** at the unlock prompt. If `/root/keyring-key.txt`
   still exists, it is in there.
3. **If the key is lost**, delete the keyring and start again:

   ```bash
   rm ~/.local/share/keyrings/login.keyring
   ```

   The account is unaffected and `gnome-keyring` recreates the keyring at the
   next login. What was in it is gone: Wi-Fi passwords, Chrome's Safe Storage
   key, GNOME Online Accounts tokens, anything Seahorse held.

---

## What it costs

A matching finger currently grants a session. With this in place it also
releases every secret in the login keyring.

That is a smaller step than it sounds. A session already gives an attacker a
great deal: Firefox's password database has no master password by default,
`~/.ssh` is readable, and `.bashrc` can be edited to wait for you. The keyring
adds Chrome's Safe Storage key, GNOME Online Accounts refresh tokens and
whatever else Seahorse holds. It is an escalation, not a new category.

**The mitigation worth knowing: only the keyring named `login` auto-unlocks.**
Create a second keyring with its own password, put high-value secrets in it, and
the fingerprint never touches them. That restores the split this otherwise
removes.

The concern is not the false accept rate. Match-on-chip sensors in this class
have no liveness detection worth the name, and this driver trusts a two byte
reply from a chip over USB because it has nothing else to go on. A false accept
rate cannot be measured on one unit, so no figure is quoted anywhere in this
repo.

---

## How it was verified

One machine, Ubuntu 24.04.4, GNOME 46, TPM 2.0, root filesystem not encrypted.

The journal across three boots, same reader, same PAM service:

```
without the module        gkr-pam: no password is available for user
                          gkr-pam: couldn't unlock the login keyring

wired, before the re-key  gkr-pam: stashed password to try later in open session
                          gkr-pam: the password for the login keyring was invalid

after the re-key          gkr-pam: stashed password to try later in open session
                          gkr-pam: unlocked login keyring
```

The middle state is the useful one. It proves the module runs and hands over a
secret, before the keyring held the matching one.

Also checked rather than assumed:

- `PAM_AUTHTOK` is 6, read from `_pam_types.h` in libpam 1.5.3-5ubuntu5.7.
- `pam_set_item` copies the value, read from `TRY_SET` in Linux-PAM v1.5.3.
- `systemd-creds` defaults to PCR 7, read from the man page on 255.4.
- The module exports `pam_sm_authenticate` and `pam_sm_setcred`, its only
  undefined symbol is `pam_set_item`, and it loads under `RTLD_NOW` with libpam
  present.
- `pamtester` against a throwaway service file, on the failing and the
  succeeding path, before anything touched GDM.

---

## Limits

One machine. One TPM. One firmware. The seal has not been tested across a
firmware update, which is the event most likely to break it on someone else's
hardware, and the reason the recovery key is written down rather than trusted to
the chip.

This is specific to `gnome-keyring`. KDE's `kwallet` has the same shape of
problem and is not addressed here.
