//! Sanity-check the SYSTEM_CPU_SET_INFORMATION parsing against real hardware.
#[cfg(windows)]
fn main() {
    use clicker_core::affinity::{enumerate_cpu_sets, select_best_cpu_set};
    let sets = enumerate_cpu_sets();
    println!("enumerated {} cpu sets", sets.len());
    for s in &sets {
        println!(
            "  id={:<10} group={} logical={:<3} efficiency_class={}",
            s.id, s.group, s.logical_index, s.efficiency_class
        );
    }
    println!("selected: {:?}", select_best_cpu_set(&sets));
}
#[cfg(not(windows))]
fn main() {}
