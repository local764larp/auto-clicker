# Measured Performance — Spec 1 Gate Artifact

**Date:** 2026-07-21
**Machine:** AMD Ryzen 5 7600 (6C/12T, homogeneous), Windows 11 IoT Enterprise LTSC 2024 (10.0.26100)
**Build:** `cargo build -p bench --release` (opt-level 3, LTO fat, codegen-units 1)
**Method:** engine pinned to cpu set 257, `THREAD_PRIORITY_TIME_CRITICAL`, fixed-point mode
targeting the centre of a topmost receiver window that counts genuinely delivered
`WM_LBUTTONDOWN`. 5 seconds per cell. Raw output: [`2026-07-21-baseline.md`](2026-07-21-baseline.md).

## Results

| Target | Batch K | Emitted CPS | Delivered CPS | Ratio |
|---|---|---|---|---|
| 50 CPS | 1 | 50 | 50 | 100.0% |
| 100 CPS | 1 | 100 | 100 | 100.0% |
| 250 CPS | 1 | 250 | 250 | 100.0% |
| 500 CPS | 1 | 500 | 500 | 100.0% |
| 1000 CPS | 1 | 1000 | 1000 | 100.0% |
| 2000 CPS | 1 | 1996 | 1996 | 100.0% |
| 5000 CPS | 1 | 3876 | 3846 | 99.2% |
| unthrottled | 1 | 3932 | 3910 | 99.4% |
| unthrottled | 2 | 4717 | 4078 | 86.5% |
| unthrottled | 4 | 5140 | 3403 | 66.2% |
| unthrottled | 8 | 5400 | 3306 | 61.2% |
| unthrottled | 16 | 5584 | 3157 | 56.5% |

## The measured ceiling

**~3,900 delivered CPS**, at `interval_ns = 0` with `K = 1`, at 99.4% fidelity.

The throttled sweep tracks its target exactly up to 2,000 CPS. A 5,000 CPS request produces
only 3,876 — the engine cannot reach it, which places the single-click ceiling just under
4,000 CPS. Unthrottled K=1 lands in the same place (3,932 emitted), confirming the limit is
`SendInput` throughput, not the scheduler.

## Batching was the wrong hypothesis

The Spec 1 design called batched `[INPUT; 2*K]` "the largest remaining user-mode throughput
win." **The measurement falsifies that.**

Batching raises *emitted* CPS monotonically — 3,932 at K=1 up to 5,584 at K=16, a 42% gain —
while *delivered* CPS **falls**: 3,910 → 3,157, and the delivery ratio collapses from 99.4%
to 56.5%. The system accepts the batched events and then coalesces or discards them
downstream. K=16 emits 42% more clicks than K=1 and delivers 19% fewer.

Peak delivered is K=2 at 4,078 CPS, a ~4% gain over K=1 bought with an 86.5% delivery ratio —
meaning one click in seven silently does not arrive. That is a bad trade for a clicker, where
a click that does not land is worse than a click not attempted.

**Batching is a vanity metric here.** It inflates the counter the tool reports to the user
while doing less actual work. `K = 1` is the correct default and the only setting with
trustworthy fidelity. The batching path stays in the code because it is measured and bounded,
but nothing should default to `K > 1`.

This is exactly the failure mode the brief warned about: a number that goes up while the
product gets worse.

## Recommended UI maximum for Spec 3

- **Slider range: 1–2,000 CPS.** Every rate in that range delivered at 100.0%.
- **Hard cap: 4,000 CPS**, flagged in the UI as beyond guaranteed delivery.
- **No unthrottled mode exposed by default**, and no batch-size control — the measurement
  shows it trades fidelity for a bigger number.
- Do not ship a million-CPS slider. The tool's real limit is under 4,000.

## Why delivered < emitted

`SendInput` serializes through the system's raw input thread, and the receiving application
consumes on its own message loop. Above roughly 2,000 CPS the receiver becomes the bottleneck,
not the engine. Any application being clicked is subject to the same limit, so the delivered
column — not the emitted one — is the number that describes what the tool actually does.

## Known limitation: the jitter distribution is not yet trustworthy

The harness reports interval percentiles, but **they measure the receiver's pump cadence, not
the engine's emission jitter.** The bench's message loop drains, then sleeps 1 ms; delivered
timestamps therefore cluster inside each pump cycle. The 2,000 CPS row makes this visible —
p50 of 37 µs against a mean of 501 µs is bimodal, which is the 1 ms pump showing through, not
the engine bursting.

The CPS columns are unaffected: they are counts over wall-clock time.

**No jitter claim should be made from this run.** Measuring emission jitter honestly requires a
timestamping wrapper `ClickSink` on the emit side, which `EngineHandle` cannot currently accept
because it hardcodes `SendInputSink`. That is tracked as follow-up work and must land before
any statement about the engine's timing distribution appears in the README.

## Reproducing

```
cargo build -p bench --release
./target/release/bench.exe > bench/results/<date>-baseline.md
```

Takes ~65 s and captures the mouse for the duration. F8 stops the engine at any point.
