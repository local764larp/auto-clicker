#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

/// Sorts `intervals` in place. The tail matters far more than the mean, so
/// p99 and max are reported alongside it and never collapsed into it.
pub fn summarize(intervals: &mut [u64]) -> Summary {
    if intervals.is_empty() {
        return Summary::default();
    }
    intervals.sort_unstable();
    let n = intervals.len();
    let sum: u128 = intervals.iter().map(|v| *v as u128).sum();

    let idx = |q: f64| -> usize {
        let i = (q * (n - 1) as f64).round() as usize;
        i.min(n - 1)
    };

    Summary {
        count: n,
        mean_ns: sum as f64 / n as f64,
        p50_ns: intervals[idx(0.50)],
        p99_ns: intervals[idx(0.99)],
        max_ns: intervals[n - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_uniform_series() {
        let mut v = vec![1000u64; 100];
        let s = summarize(&mut v);
        assert_eq!(s.count, 100);
        assert_eq!(s.p50_ns, 1000);
        assert_eq!(s.p99_ns, 1000);
        assert_eq!(s.max_ns, 1000);
        assert!((s.mean_ns - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn exposes_the_tail() {
        // 99 fast samples and one 100x outlier: the mean hides it, p99 and max
        // must not.
        let mut v = vec![1000u64; 99];
        v.push(100_000);
        let s = summarize(&mut v);
        assert_eq!(s.max_ns, 100_000);
        assert_eq!(s.p50_ns, 1000);
        assert!(s.mean_ns < 2000.0, "mean alone would hide the outlier");
    }

    #[test]
    fn empty_input_is_all_zero() {
        let s = summarize(&mut []);
        assert_eq!(s.count, 0);
        assert_eq!(s.p50_ns, 0);
        assert_eq!(s.max_ns, 0);
    }

    #[test]
    fn percentiles_are_order_independent() {
        let mut a = vec![5u64, 1, 4, 2, 3];
        let mut b = vec![1u64, 2, 3, 4, 5];
        assert_eq!(summarize(&mut a).p50_ns, summarize(&mut b).p50_ns);
    }
}
