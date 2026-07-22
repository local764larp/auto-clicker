engine pinned to core Some(257)
receiver at (200, 200), clicking its centre (500, 400)

  50 CPS             emitted        50 CPS   delivered        50 CPS
  100 CPS            emitted       100 CPS   delivered       100 CPS
  250 CPS            emitted       250 CPS   delivered       250 CPS
  500 CPS            emitted       500 CPS   delivered       500 CPS
  1000 CPS           emitted      1000 CPS   delivered      1000 CPS
  2000 CPS           emitted      2000 CPS   delivered      2000 CPS
  5000 CPS           emitted      3892 CPS   delivered      3871 CPS
  unthrottled  K=1   emitted      3959 CPS   delivered      3929 CPS
  unthrottled  K=2   emitted      4611 CPS   delivered      4059 CPS
  unthrottled  K=4   emitted      5114 CPS   delivered      3416 CPS
  unthrottled  K=8   emitted      5331 CPS   delivered      3339 CPS
  unthrottled  K=16  emitted      5386 CPS   delivered      3091 CPS

### Throughput

| Target | Batch K | Emitted CPS | Delivered CPS | Ratio |
|---|---|---|---|---|
| 50 CPS | 1 | 50 | 50 | 100.0% |
| 100 CPS | 1 | 100 | 100 | 100.0% |
| 250 CPS | 1 | 250 | 250 | 100.0% |
| 500 CPS | 1 | 500 | 500 | 100.0% |
| 1000 CPS | 1 | 1000 | 1000 | 100.0% |
| 2000 CPS | 1 | 2000 | 2000 | 100.0% |
| 5000 CPS | 1 | 3892 | 3871 | 99.5% |
| unthrottled | 1 | 3959 | 3929 | 99.2% |
| unthrottled | 2 | 4611 | 4059 | 88.0% |
| unthrottled | 4 | 5114 | 3416 | 66.8% |
| unthrottled | 8 | 5331 | 3339 | 62.6% |
| unthrottled | 16 | 5386 | 3091 | 57.4% |

### Emit-side interval distribution (the engine's own timing)

| Target | Batch K | samples | mean us | p50 us | p99 us | max us |
|---|---|---|---|---|---|---|
| 50 CPS | 1 | 250 | 20000.0 | 20000.0 | 20000.7 | 20019.4 |
| 100 CPS | 1 | 499 | 10000.0 | 10000.0 | 10000.6 | 10039.7 |
| 250 CPS | 1 | 1249 | 4000.0 | 4000.0 | 4000.1 | 4003.8 |
| 500 CPS | 1 | 2500 | 2000.0 | 2000.0 | 2000.1 | 2360.5 |
| 1000 CPS | 1 | 5000 | 1000.0 | 1000.0 | 1000.1 | 1114.7 |
| 2000 CPS | 1 | 9999 | 500.0 | 500.0 | 500.1 | 1200.1 |
| 5000 CPS | 1 | 20124 | 256.9 | 245.6 | 458.8 | 1624.8 |
| unthrottled | 1 | 20322 | 252.6 | 240.4 | 452.9 | 2054.9 |
| unthrottled | 2 | 26697 | 216.8 | 0.0 | 652.6 | 2107.7 |
| unthrottled | 4 | 29123 | 195.5 | 0.0 | 1018.7 | 2030.6 |
| unthrottled | 8 | 26999 | 187.5 | 0.0 | 1731.2 | 2573.6 |
| unthrottled | 16 | 29135 | 185.6 | 0.0 | 3161.4 | 4558.2 |

### Delivered-side interval distribution (receiver, NOT the engine)

| Target | Batch K | samples | mean us | p50 us | p99 us | max us |
|---|---|---|---|---|---|---|
| 50 CPS | 1 | 250 | 19981.7 | 19993.2 | 21142.5 | 21322.6 |
| 100 CPS | 1 | 499 | 9998.8 | 9709.8 | 11198.0 | 11444.3 |
| 250 CPS | 1 | 1249 | 4000.2 | 4295.1 | 5259.3 | 5441.5 |
| 500 CPS | 1 | 2500 | 1999.7 | 1577.0 | 3381.8 | 3883.0 |
| 1000 CPS | 1 | 5000 | 999.9 | 1550.7 | 2075.0 | 2624.9 |
| 2000 CPS | 1 | 9999 | 500.0 | 37.3 | 1929.7 | 2377.3 |
| 5000 CPS | 1 | 20014 | 262.7 | 335.8 | 625.2 | 1971.2 |
| unthrottled | 1 | 20166 | 258.9 | 333.7 | 619.0 | 2215.9 |
| unthrottled | 2 | 23499 | 249.7 | 261.7 | 708.3 | 2291.1 |
| unthrottled | 4 | 19452 | 297.0 | 374.5 | 842.8 | 2519.7 |
| unthrottled | 8 | 16911 | 304.5 | 392.3 | 890.2 | 2497.5 |
| unthrottled | 16 | 16720 | 328.8 | 430.8 | 968.6 | 2624.5 |

Delivered below emitted is expected, not a bug: SendInput serializes
through the system raw input thread and the receiver consumes on its
own message loop.

The delivered-side distribution is bounded by this harness's ~1ms pump
cadence and describes the RECEIVER. Use the emit-side table to
characterise the engine.
