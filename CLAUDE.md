
# CLAUDE.md

Rules for working in this repo. Read this before `plan.md`, every session.

`elanmoc-rs` is a userspace fingerprint driver for the ELAN 04f3:0c90 sensor.
Pure Rust. Ubuntu 24.04. It talks to real hardware that can be destroyed by bad bytes.

---

## 1. Device safety

This is the only section where breaking a rule is unrecoverable. Everything else is style.

The sensor is an ARM Cortex-M4 with writable flash, soldered to the laptop board. There is no reflash path. A wrong command can end the project and cost the user a hardware repair.

| Never                                                                              | Why                                                          |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Send bytes not present in`docs/protocol.md`                                      | Unknown opcodes can hit flash write or bootloader entry.     |
| Add a command to`docs/protocol.md` yourself                                      | That file is sourced by the user, not derived by you.        |
| Send anything from the DO NOT SEND table in`docs/protocol.md`                    | Bootloader entry, register writes, EC pin control.           |
| Loop over opcode values                                                            | That is fuzzing a flash-capable MCU.                         |
| Retry a failed command with modified bytes                                         | A wrong guess twice is still a wrong guess.                  |
| Call`wipe_all`, `delete` or `delete_subsid` outside an explicit user request | No undo.                                                     |
| Call any destructive command in a test or in cleanup                               | Tests run unattended.                                        |
| Leave a session open after an error                                                | Send`abort` (`40 ff 02`) before releasing the interface. |

If a task needs a command that is not in the table: **stop, report the gap, wait.** Do not work around it.

Commands marked `destructive` in `docs/protocol.md` require a GATE (see section 3) before their first run in any session.

---

## 2. Rust

| Rule                                                   | Limit                                                                      |
| ------------------------------------------------------ | -------------------------------------------------------------------------- |
| No C dependencies                                      | `nusb` not `rusb`. No `pam-sys`, no `glib`, no `-sys` crates.    |
| No`unwrap()` or `expect()` outside tests           | One`thiserror` enum per crate, `#[from]` upward.                       |
| No`Box<dyn Error>` in library crates                 | `anyhow` allowed in `elanmoc-cli` and `elanmocd` binaries only.      |
| Every USB read has an explicit timeout                 | Per command, from`Command::timeout()`. Never a global default.           |
| Every wait is cancellable                              | `tokio::select!` against a `CancellationToken`. No exceptions.         |
| Protocol encoding is pure                              | `elanmoc-proto` must not depend on `elanmoc-usb`. Bytes in, bytes out. |
| `cargo clippy -- -D warnings` passes                 | Before every commit.                                                       |
| No`#[allow(...)]` without a one-line reason above it |                                                                            |

Encode and parse functions take and return byte slices. If you cannot unit test a protocol function without hardware attached, it is in the wrong crate.

---

## 3. How to work

**One task at a time. Stop and report after each one.** Do not chain phases.

**GATEs are hard stops.** `plan.md` has seven: 0, 1, 2, 3a, 3b, 6, 7. At each one: report status, wait for the user, do not proceed on your own judgement. A gate is not a checkpoint to narrate past.

**Four files at the repo root, maintained by you:**

| File                 | What goes in it                                                                           |
| -------------------- | ----------------------------------------------------------------------------------------- |
| `PROGRESS.md`      | Append one line per completed task. Date, task, outcome. Never rewrite history.           |
| `QUESTIONS.md`     | Anything blocking that needs the user. Numbered Q-001, Q-002.                             |
| `DECISIONS.md`     | Choices made and why. Numbered D-001. Includes anything that contradicts`plan.md`.      |
| `BACKLOG.md`       | Things worth doing later. Not now.                                                        |
| `docs/findings.md` | Append-only. Every observed device response, every difference from the documented source. |

`docs/findings.md` is the scientific record of this project. When a response does not match the documented layout, **the device is right and the document is wrong.** Record the difference. Do not bend the parser to fit.

**Read each session:** `CLAUDE.md`, `docs/protocol.md`, the current phase of `plan.md`, last 20 lines of `PROGRESS.md`.

---

## 4. Comments

- Default is NO comment. The code should explain itself.
- Write one only if removing it would cause someone to break the code.
- Max 10 words. One line. Never a paragraph.
- Never restate a function or variable name.
- No banners or dividers (`// ==== Setup ====`).
- No phase, step or stage numbers (`// Phase 3`).
- No `TODO` / `FIXME` without a `Q-NNN` reference.
- No commented-out code. Delete it.
- No comments on code you did not change.
- More than 3 comments in a file means the code is unclear. Rewrite it.

One exception: a magic byte value gets a comment naming the command it belongs to.

```rust
const CMD_ABORT: &[u8] = &[0x40, 0xff, 0x02];  // abort, per protocol.md
```

---

## 5. Commits

```
type(scope): short subject in plain English

- what changed
- what changed
```

**Types:** `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `ci`, `perf`.

**Scopes, use only these:** `usb`, `proto`, `enroll`, `verify`, `store`, `daemon`, `dbus`, `pam`, `cli`, `udev`, `polkit`, `systemd`, `docs`, `deps`, `ci`.

**Subject:** imperative, lowercase, no full stop, title line under 60 characters. Say what changed, not what you did. `add enroll state machine`, not `added the enroll state machine`.

**Body:** bullets only, max 5, one line each. Skip it if the title says everything.

**Never** mention Claude, AI or any tool. No `Co-Authored-By`. No `Generated with`. No emoji.

### Good

```
feat(proto): add enroll state machine

- EnrollState covers idle, awaiting touch, committing, done
- step() is pure, takes response bytes, returns next action
- replay test drives it from a recorded capture
```

```
fix(usb): read touch-wait replies on the correct endpoint

- verify and enroll reply on 0x84, not 0x83
- was a timeout on every enroll sample
```

```
docs(proto): confirm fw_ver response layout on 0c90
```

### Bad

```
feat: Implemented comprehensive USB transport layer with robust error
handling and seamless cancellation support

Co-Authored-By: Claude <noreply@anthropic.com>
```

```
WIP
fix various issues
update stuff
```

### Before every commit

- `cargo build` passes
- `cargo test` passes
- `cargo clippy -- -D warnings` passes
- `git diff --staged` reviewed, nothing unrelated staged
- no captures, pcaps or `target/` staged

---

## 6. System resources

This runs on the user's daily laptop. A frozen desktop costs them their open work, which is worse than any delay.

**Measure, do not assume.** Do not trust any number written in this file. Before starting anything heavy, run the check:

```bash
free -m | awk '/Mem:/ {print "free:", $7"MB"}'; uptime | grep -o 'load average.*'; nproc
```

Rules:

| Rule                                       | Detail                                                                            |
| ------------------------------------------ | --------------------------------------------------------------------------------- |
| Reserve 3 GB for the desktop               | Never budget past`MemAvailable` minus 3 GB.                                     |
| One heavy process at a time                | The budget is the machine's, not per agent.                                       |
| Never`cargo build -j` with all threads   | Leave at least 2 cores free.                                                      |
| Refuse rather than degrade                 | If the budget is below floor, stop and say so. Do not start a job that will swap. |
| No Docker, no database, no service install | This project needs none. If you think it does, that is a`QUESTIONS.md` entry.   |

**Worker agents:** you decide how many and for what. One constraint: only one may run a build or a device operation at a time. Workers produce files. You review and commit. Workers never commit and never touch the device.

---

## 7. Never touch without a GATE

| Path                               | Why                                                                          |
| ---------------------------------- | ---------------------------------------------------------------------------- |
| `/etc/pam.d/*`                   | A mistake locks the user out of their own machine. Phase 7 only.             |
| `/etc/udev/rules.d/*`            | Show the rule first.                                                         |
| `docs/protocol.md` command table | User-sourced. You may only change`status:` and fill in observed responses. |
| `systemctl mask` / `unmask`    | Say which unit and why before running it.                                    |
| Anything that writes device flash  | Section 1.                                                                   |

---

## 8. Writing

Plain, short English. Same rules for commits, `PROGRESS.md`, docs and replies.

- No em dashes. Use commas, periods, colons or parentheses.
- No buzzwords, no filler, no "comprehensive", "robust", "seamless", "leverage".
- Short sentences. Say it simply or do not say it.
- State findings as facts with evidence, not as confidence. "Chip returned `00 fd` after sample 8" beats "enrollment is working well".
- Never invent a number. If you did not observe it, do not write it.

---

## 9. Anti-patterns

| Never                                                       | Why                                                                                  |
| ----------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Guess a byte value                                          | Section 1. This is the one that ends the project.                                    |
| Say a phase is done without running its acceptance criteria | `plan.md` lists them per phase. Run them.                                          |
| Roll past a GATE                                            | The gate exists because the next step is expensive or irreversible.                  |
| Bend a parser to match the documentation                    | The device is the truth. Record the difference.                                      |
| Collapse the three IN endpoints into one                    | `0x82` image, `0x83` status, `0x84` touch-wait. They are different on purpose. |
| Hardcode`total_attempts = 8`                              | It came from a different chip. Confirm it on this one.                               |
| Write image processing code                                 | The chip matches on-chip.`capture` is debug only.                                  |
| Add a dependency to fix a small problem                     | Ask first.                                                                           |
| Save up work for one big commit                             | The history is part of what gets reviewed.                                           |
| Report success when a workaround was needed                 | If enroll needs a cancel to save, enroll is broken. Say so.                          |
