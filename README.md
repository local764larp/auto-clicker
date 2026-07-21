# Auto Clicker

A Windows auto clicker built to reach the genuine user-mode throughput ceiling, with a
hand-rendered neumorphic Direct2D interface. Written in Rust against raw Win32 — no GUI
framework.

**Status:** the click engine and its measurement harness are complete and merged. The interface
is in progress.

## Measured performance

Every number here comes from [`bench/`](bench/) on an AMD Ryzen 5 7600 (6C/12T) under
Windows 11 26100. Full write-up and method: [`bench/results/README.md`](bench/results/README.md).

| Target rate | Emitted CPS | Delivered CPS | Delivery |
|---|---|---|---|
| 50 – 1000 CPS | exact | exact | **100.0%** |
| 2000 CPS | 1999 | 1999 | **100.0%** |
| 5000 CPS (requested) | 4016 | 3983 | 99.2% |
| unthrottled | 4017 | 3982 | 99.1% |

**The ceiling is ~4,000 delivered clicks per second.** Not a million. `SendInput` serializes
through the system's raw input thread and the receiving application consumes on its own message
loop, so above roughly 2,000 CPS the receiver — not this tool — is the bottleneck. Any clicker on
Windows is subject to the same limit; most simply don't measure it.

### Scheduling accuracy

Measured inside the engine, on the same clock the scheduler uses:

| Target | mean | p50 | p99 | max |
|---|---|---|---|---|
| 50 CPS (20,000 µs) | 20000.0 µs | 20000.0 µs | 20000.2 µs | 20000.6 µs |
| 1000 CPS (1,000 µs) | 1000.0 µs | 1000.0 µs | 1002.2 µs | 1058.1 µs |

Sub-microsecond at 50 CPS across 249 samples; 0.2% off target at p99 at 1,000 CPS. The scheduler
advances an absolute deadline by the interval rather than computing `now + interval`, so
per-iteration overhead cannot accumulate into drift.

### Two findings worth stating

**Batching multiple clicks per syscall is a vanity metric.** `SendInput` accepts an array, so
`[INPUT; 2*K]` submits K clicks in one call — and it does raise *emitted* CPS by 36%. But
*delivered* CPS **falls** 22%, with fidelity collapsing from 99.1% to 57.0%. The emit-side probe
shows why: p50 interval is exactly `0.0 µs` for every K > 1, because batched clicks carry no
spacing at all, and the input stack coalesces them. It makes the counter bigger while doing less
real work. The default is K = 1.

**The engine loop is not the bottleneck.** Against a null sink it sustains ~37 million CPS —
roughly four orders of magnitude above the delivered ceiling. All of the limit is `SendInput`.

## Design

The engine is a pinned, `TIME_CRITICAL` native thread that allocates nothing, locks nothing, and
logs nothing in its hot loop. It communicates with the rest of the program exclusively through a
block of atomics, so the interface can never perturb its timing.

- **Three-tier hybrid wait** — high-resolution waitable timer above 2 ms, `SwitchToThread` yield
  loop from 50 µs to 2 ms, `spin_loop` below that. The default 15.6 ms Windows timer tick would
  otherwise cap throughput near 64 CPS.
- **Deterministic tests.** The engine is generic over `Clock`, `Waiter`, and `ClickSink`, so the
  entire loop — limits, drift, snap-forward, unthrottled mode — is tested in virtual time with no
  sleeping and no tolerance windows. 60 tests run in ~2 s.
- **Emergency stop is a safety requirement, not a feature.** A dedicated F8 hotkey lives on its
  own message-only window thread and clears the running flag directly on the shared atomic — no
  queue, no channel, no dependency on the interface being responsive. The engine refuses to start
  if it cannot register the hotkey.
- **Bounded shutdown.** `SendInput` can block when the target's input queue saturates, so
  shutdown waits with a timeout and detaches rather than hanging. An unkillable clicker is the
  exact failure mode the emergency stop exists to prevent.

`clicker-core` carries zero GUI dependencies and its timing logic compiles for non-Windows
targets, which is what keeps it testable.

## Layout

```
crates/clicker-core/   engine, timing, sinks — no GUI dependencies
crates/clicker-gui/    Win32 + Direct2D interface (in progress)
bench/                 measurement harness and recorded results
docs/superpowers/      design specs and implementation plans
```

## Building

Requires Rust 1.96+ and the MSVC toolchain (`x86_64-pc-windows-msvc`).

```
cargo build --release
cargo test --workspace
```

Reproduce the measurements:

```
cargo build -p bench --release
./target/release/bench.exe
```

Takes ~65 s and captures the mouse. F8 stops the engine at any point.

## Scope

Out of scope by design: kernel-mode drivers, HID emulation via unsigned drivers, anti-cheat
evasion, injection-signature masking, and timing humanization intended to defeat detection. The
goal is **throughput, not concealment**. Anything whose primary value is being harder to detect
does not belong here.

## Licence

Not yet chosen.
