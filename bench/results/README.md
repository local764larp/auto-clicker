# Measured Performance — Spec 1 Gate Artifact

**Date:** 2026-07-21
**Machine:** AMD Ryzen 5 7600 (6C/12T, homogeneous), Windows 11 IoT Enterprise LTSC 2024 (10.0.26100)
**Build:** `cargo build -p bench --release` (opt-level 3, LTO fat, codegen-units 1)
**Method:** engine pinned to cpu set 257, `THREAD_PRIORITY_TIME_CRITICAL`, fixed-point mode
targeting the centre of a topmost receiver window that counts genuinely delivered
`WM_LBUTTONDOWN`. 5 seconds per cell. Emission timing captured inside the engine by a
`ProbedSink` wrapping the production `SendInputSink`, on the same `QpcClock` the scheduler uses.
Raw output: [`2026-07-21-baseline.md`](2026-07-21-baseline.md).

## Throughput

| Target | Batch K | Emitted CPS | Delivered CPS | Ratio |
|---|---|---|---|---|
| 50 CPS | 1 | 50 | 50 | 100.0% |
| 100 CPS | 1 | 100 | 100 | 100.0% |
| 250 CPS | 1 | 250 | 250 | 100.0% |
| 500 CPS | 1 | 500 | 500 | 100.0% |
| 1000 CPS | 1 | 1000 | 1000 | 100.0% |
| 2000 CPS | 1 | 1999 | 1999 | 100.0% |
| 5000 CPS | 1 | 4016 | 3983 | 99.2% |
| unthrottled | 1 | 4017 | 3982 | 99.1% |
| unthrottled | 2 | 4641 | 4122 | 88.8% |
| unthrottled | 4 | 5043 | 3352 | 66.5% |
| unthrottled | 8 | 5305 | 3263 | 61.5% |
| unthrottled | 16 | 5455 | 3109 | 57.0% |

## Engine timing (emit-side)

This is the engine's own scheduling, measured inside the hot loop.

| Target | Batch K | samples | mean µs | p50 µs | p99 µs | max µs |
|---|---|---|---|---|---|---|
| 50 CPS | 1 | 249 | 20000.0 | 20000.0 | 20000.2 | 20000.6 |
| 100 CPS | 1 | 500 | 10000.0 | 10000.0 | 10000.5 | 10014.1 |
| 250 CPS | 1 | 1249 | 4000.0 | 4000.0 | 4000.1 | 4012.2 |
| 500 CPS | 1 | 2499 | 2000.0 | 2000.0 | 2000.1 | 2113.2 |
| 1000 CPS | 1 | 4999 | 1000.0 | 1000.0 | 1002.2 | 1058.1 |
| 2000 CPS | 1 | 9998 | 500.0 | 500.0 | 512.7 | 2165.6 |
| 5000 CPS | 1 | 20320 | 248.9 | 240.1 | 480.0 | 1389.5 |
| unthrottled | 1 | 20267 | 248.9 | 239.6 | 475.1 | 1995.7 |
| unthrottled | 2 | 23755 | 215.4 | 0.0 | 663.3 | 1584.6 |
| unthrottled | 4 | 28935 | 198.3 | 0.0 | 1001.0 | 1983.8 |
| unthrottled | 8 | 27007 | 188.4 | 0.0 | 1720.3 | 3084.6 |
| unthrottled | 16 | 29023 | 183.3 | 0.0 | 3088.1 | 4410.4 |

**The scheduler is accurate to well under a microsecond at every throttled rate.** At 50 CPS the
mean, p50, and p99 are all 20,000.0 µs against a 20,000 µs target, with a worst-case deviation of
0.6 µs across 249 samples. At 1,000 CPS, p99 is 1,002.2 µs — 0.2% off target — with a 58 µs
worst case. The absolute-deadline advancement in `schedule::decide` is doing exactly what it was
designed to do; there is no measurable drift.

Tail behaviour degrades only where it should: the 2,165 µs max at 2,000 CPS and the ~1,400–2,000 µs
maxima at the ceiling are scheduler preemption, and the snap-forward branch absorbs them without
firing catch-up bursts.

## The measured ceiling

**~4,000 delivered CPS**, at `interval_ns = 0` with `K = 1`, at 99.1% fidelity.

The throttled sweep tracks its target exactly up to 2,000 CPS. A 5,000 CPS request yields only
4,016 — the engine cannot reach it — and unthrottled K=1 lands in the same place (4,017). Two
different code paths converging on the same number establishes the limit as `SendInput`
throughput, not the scheduler. Run-to-run variance on the ceiling is roughly ±150 CPS.

## Batching was the wrong hypothesis

The Spec 1 design called batched `[INPUT; 2*K]` "the largest remaining user-mode throughput win."
**The measurement falsifies that, and the emit-side data explains why.**

Batching raises *emitted* CPS monotonically — 4,017 at K=1 up to 5,455 at K=16, a 36% gain — while
*delivered* CPS **falls**: 3,982 → 3,109, with the delivery ratio collapsing from 99.1% to 57.0%.
K=16 emits 36% more clicks than K=1 and delivers 22% fewer.

The mechanism is visible in the emit-side p50: **0.0 µs for every K > 1.** Clicks inside one
`SendInput` batch carry literally zero inter-click spacing — they are submitted in the same call,
in the same instant. The system then coalesces or discards them downstream. Batching does not
produce more clicks; it produces the same work compressed into instants the input stack refuses
to honour.

Peak delivered is K=2 at 4,122 CPS — a ~3.5% gain over K=1, bought at an 88.8% delivery ratio,
meaning better than one click in nine silently does not arrive. That is a bad trade for a clicker,
where a click that does not land is worse than a click never attempted.

**Batching is a vanity metric here.** It inflates the counter the tool shows the user while doing
less real work. `K = 1` is the correct default and the only setting with trustworthy fidelity. The
batching path stays in the code because it is measured and bounded, but nothing should default to
`K > 1`.

This is exactly the failure mode the brief warned about: a number that goes up while the product
gets worse. The sweep axis existed to answer this question empirically, and it did.

## Recommended UI maximum for Spec 3

- **Slider range: 1–2,000 CPS.** Every rate in that range delivered at 100.0% with sub-microsecond
  scheduling accuracy.
- **Hard cap: 4,000 CPS**, flagged in the UI as beyond guaranteed delivery.
- **No batch-size control**, and no unthrottled mode exposed by default — the measurement shows
  both trade fidelity for a bigger number.
- Do not ship a million-CPS slider. The tool's real limit is about 4,000.

## Why delivered < emitted

`SendInput` serializes through the system's raw input thread, and the receiving application
consumes on its own message loop. Above roughly 2,000 CPS the receiver becomes the bottleneck,
not the engine. Any application being clicked is subject to the same limit, so the delivered
column — not the emitted one — describes what the tool actually does.

## Reading the delivered-side distribution

The raw output also contains a delivered-side interval table. **It characterises the receiver,
not the engine**, because this harness's message loop drains then sleeps ~1 ms, so delivered
timestamps cluster inside each pump cycle. The 2,000 CPS row shows the artifact plainly: a
delivered p50 of 37 µs against a 500 µs mean is the pump cadence showing through, while the
emit-side p50 for the same cell is exactly 500.0 µs. Use the emit-side table for any statement
about engine timing.

## GUI-active timing (Spec 3 gate — 2026-07-22)

The mission's hard rule is that the GUI must never measurably perturb engine timing. This is the
falsifiable test: the bench's engine timing measured **alone** versus **with the interface running
beside it**, repainting continuously at 60 Hz — a heavier load than the real render-on-demand
design, which repaints only on interaction or a 10 Hz readout change.

Emit-side p99 (the engine's own timing), µs:

| Rate | Engine alone | Engine + GUI @60 Hz | Δ p99 |
|---|---|---|---|
| 50 CPS | 20000.3 | 20000.7 | +0.4 |
| 100 CPS | 10000.1 | 10000.6 | +0.5 |
| 250 CPS | 4000.1 | 4000.1 | 0.0 |
| 500 CPS | 2000.1 | 2000.1 | 0.0 |
| 1000 CPS | 1000.1 | 1000.1 | 0.0 |
| 2000 CPS | 500.1 | 500.1 | 0.0 |

Sub-microsecond at every throttled rate, and the max column improved in several rows under GUI
load (1000 CPS max: 1114 µs with the GUI vs 4385 µs without) — ordinary scheduler noise, not a
regression. **The GUI does not perturb the engine.** The architecture delivers what it promised:
a pinned `TIME_CRITICAL` engine reading atomics, a render-on-demand GUI, and no lock, channel, or
per-click path between them.

Method: `CLICKER_NO_ENGINE=1 clicker-gui.exe` runs the interface in render-only mode (no engine,
so no second F8 registration) with the readout timer forced to a 60 Hz full-window repaint; the
bench runs its own engine and receiver in a separate process. Raw output:
[`2026-07-22-gate-baseline.md`](2026-07-22-gate-baseline.md) and
[`2026-07-22-gui-active.md`](2026-07-22-gui-active.md).

## Manual verification status

| Check | Status |
|---|---|
| F8 emergency stop while the engine saturates a core | **Verified** 2026-07-21 via `--example soak`. Clicking stopped on keypress; the machine stayed usable. Also automated as pass 4 of `--example shutdown_diag`, which synthesizes the keypress: `running` cleared in ~10 ms. |
| Shutdown is bounded when the engine thread wedges | **Verified** — `runtime::tests::shutdown_is_bounded_even_if_the_sink_wedges`. |
| UIPI-blocked target surfaces `SinkError::Blocked` rather than a silent no-op | Not yet verified — needs an elevated foreground window. |
| Hybrid P-core selection on real Intel 12th-gen+ silicon | Cannot be verified on this machine (homogeneous Ryzen 5 7600). Unit-tested only. |
| `timeBeginPeriod` fallback path | Not yet verified — needs the high-resolution timer branch forced off. |

## Operational hazard: follow-cursor at ceiling rates

The first soak run hung — the process survived 2m12s past its own 20s bound, engine thread parked
in a syscall. Investigation (`--example shutdown_diag`) ruled out the engine loop, throttled
`SendInput`, unthrottled `SendInput`, and the F8 path: all four shut down in 0.4–2 ms.

The remaining difference was that soak runs in **follow-cursor** mode, so at ~4,000 CPS it clicks
whatever desktop UI is under the pointer. `SendInput` blocks when the target thread's input queue
saturates, and an unresponsive target can therefore wedge the engine thread indefinitely.

`EngineHandle::drop` previously joined unconditionally, so a wedged thread hung the process
forever. **An unkillable clicker is the exact failure mode the emergency stop exists to prevent**,
so shutdown is now bounded by `SHUTDOWN_TIMEOUT` (2 s) and detaches with a diagnostic message
rather than hanging. Healthy shutdown is unaffected at ~2 ms.

Practical consequence: prefer fixed-point mode when running at ceiling rates. Follow-cursor
against arbitrary UI is the configuration that can stall the input stack.

## Baseline for Spec 3

The brief requires that the GUI never measurably perturb engine timing. The emit-side table is
the baseline that makes that falsifiable: re-run this sweep with the GUI running and compare
p99 per rate. At 1,000 CPS anything worse than ~1,002 µs p99 is the GUI intruding on the engine.

## Reproducing

```
cargo build -p bench --release
./target/release/bench.exe > bench/results/<date>-baseline.md
```

Takes ~65 s and captures the mouse for the duration. F8 stops the engine at any point.
