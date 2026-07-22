engine pinned to core Some(257)
receiver at (200, 200), clicking its centre (500, 400)

  50 CPS             emitted        50 CPS   delivered        50 CPS
  100 CPS            emitted       100 CPS   delivered       100 CPS
  250 CPS            emitted       250 CPS   delivered       250 CPS
  500 CPS            emitted       500 CPS   delivered       500 CPS
  1000 CPS           emitted      1000 CPS   delivered      1000 CPS
  2000 CPS           emitted      2000 CPS   delivered      2000 CPS
  5000 CPS           emitted      3924 CPS   delivered      3907 CPS
  unthrottled  K=1   emitted      4027 CPS   delivered      3980 CPS
  unthrottled  K=2   emitted      4744 CPS   delivered      4122 CPS
  unthrottled  K=4   emitted      5123 CPS   delivered      3301 CPS
  unthrottled  K=8   emitted      5309 CPS   delivered      3253 CPS
  unthrottled  K=16  emitted      5434 CPS   delivered      3079 CPS

### Throughput

| Target | Batch K | Emitted CPS | Delivered CPS | Ratio |
|---|---|---|---|---|
| 50 CPS | 1 | 50 | 50 | 100.0% |
| 100 CPS | 1 | 100 | 100 | 100.0% |
| 250 CPS | 1 | 250 | 250 | 100.0% |
| 500 CPS | 1 | 500 | 500 | 100.0% |
| 1000 CPS | 1 | 1000 | 1000 | 100.0% |
| 2000 CPS | 1 | 2000 | 2000 | 100.0% |
| 5000 CPS | 1 | 3924 | 3907 | 99.6% |
| unthrottled | 1 | 4027 | 3980 | 98.8% |
| unthrottled | 2 | 4744 | 4122 | 86.9% |
| unthrottled | 4 | 5123 | 3301 | 64.4% |
| unthrottled | 8 | 5309 | 3253 | 61.3% |
| unthrottled | 16 | 5434 | 3079 | 56.7% |

### Emit-side interval distribution (the engine's own timing)

| Target | Batch K | samples | mean us | p50 us | p99 us | max us |
|---|---|---|---|---|---|---|
| 50 CPS | 1 | 250 | 20000.0 | 20000.0 | 20000.3 | 20122.3 |
| 100 CPS | 1 | 499 | 10000.0 | 10000.0 | 10000.1 | 10000.4 |
| 250 CPS | 1 | 1249 | 4000.0 | 4000.0 | 4000.1 | 4000.8 |
| 500 CPS | 1 | 2500 | 2000.0 | 2000.0 | 2000.1 | 7230.7 |
| 1000 CPS | 1 | 4999 | 1000.0 | 1000.0 | 1000.1 | 4385.3 |
| 2000 CPS | 1 | 10000 | 500.0 | 500.0 | 500.1 | 1540.1 |
| 5000 CPS | 1 | 19742 | 254.8 | 244.5 | 428.2 | 1861.8 |
| unthrottled | 1 | 20712 | 248.2 | 239.0 | 417.9 | 1594.6 |
| unthrottled | 2 | 23955 | 210.8 | 0.0 | 615.4 | 1975.9 |
| unthrottled | 4 | 29563 | 195.2 | 0.0 | 1001.2 | 1857.1 |
| unthrottled | 8 | 27223 | 188.3 | 0.0 | 1738.0 | 2763.7 |
| unthrottled | 16 | 29055 | 183.9 | 0.0 | 3120.0 | 4114.1 |

### Delivered-side interval distribution (receiver, NOT the engine)

| Target | Batch K | samples | mean us | p50 us | p99 us | max us |
|---|---|---|---|---|---|---|
| 50 CPS | 1 | 250 | 19998.2 | 19935.2 | 21503.1 | 21600.0 |
| 100 CPS | 1 | 499 | 10000.0 | 9652.8 | 11104.0 | 15537.2 |
| 250 CPS | 1 | 1249 | 3999.4 | 4559.7 | 5182.4 | 5471.5 |
| 500 CPS | 1 | 2500 | 1999.5 | 1577.5 | 3286.6 | 7882.9 |
| 1000 CPS | 1 | 4999 | 1000.0 | 1549.3 | 1928.6 | 4643.4 |
| 2000 CPS | 1 | 10000 | 500.0 | 46.3 | 1902.7 | 2572.5 |
| 5000 CPS | 1 | 19657 | 260.4 | 331.1 | 587.5 | 1974.6 |
| unthrottled | 1 | 20466 | 255.8 | 334.8 | 593.6 | 1906.6 |
| unthrottled | 2 | 20814 | 246.6 | 345.8 | 652.9 | 2066.1 |
| unthrottled | 4 | 19047 | 307.3 | 389.6 | 886.1 | 1994.5 |
| unthrottled | 8 | 16678 | 312.5 | 413.3 | 898.3 | 2306.0 |
| unthrottled | 16 | 16462 | 329.9 | 442.8 | 967.7 | 2146.5 |

Delivered below emitted is expected, not a bug: SendInput serializes
through the system raw input thread and the receiver consumes on its
own message loop.

The delivered-side distribution is bounded by this harness's ~1ms pump
cadence and describes the RECEIVER. Use the emit-side table to
characterise the engine.
