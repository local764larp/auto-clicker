//! Thread priority and core pinning.
//!
//! NOTE: the hybrid (P-core vs E-core) selection branch of
//! `select_best_cpu_set` is exercised by unit tests but has never been
//! validated against real hybrid silicon — the development machine is a
//! homogeneous AMD Ryzen 5 7600 where every cpu set reports
//! `efficiency_class == 0`. Treat the hybrid path as unverified on hardware.

use windows::Win32::System::SystemInformation::GetSystemCpuSetInformation;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetThreadPriority, SetThreadPriority,
    SetThreadSelectedCpuSets, THREAD_PRIORITY, THREAD_PRIORITY_TIME_CRITICAL,
};

/// Restores the thread's previous priority on drop.
#[derive(Debug)]
pub struct ThreadPriorityGuard {
    previous: i32,
}

impl ThreadPriorityGuard {
    pub fn time_critical() -> Self {
        // SAFETY: GetCurrentThread returns a pseudo-handle that needs no
        // closing and is always valid on the calling thread. Both calls take
        // that handle plus a plain integer.
        let previous = unsafe { GetThreadPriority(GetCurrentThread()) };
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        }
        Self { previous }
    }
}

impl Drop for ThreadPriorityGuard {
    fn drop(&mut self) {
        // SAFETY: as above. `previous` is whatever GetThreadPriority returned,
        // so it is a value the API already produced for this thread.
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(self.previous));
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CpuSet {
    pub id: u32,
    /// Higher is more performant. On Intel hybrid parts P-cores report a
    /// higher class than E-cores; on homogeneous parts every set reports 0.
    pub efficiency_class: u8,
    pub group: u16,
    pub logical_index: u8,
}

/// Pure selection policy, unit tested independently of the OS.
///
/// Prefers the highest `efficiency_class`. Among equals, prefers the
/// lowest-numbered set that is not the first, because core 0 absorbs more
/// interrupt and DPC work than its siblings. Deterministic by construction.
pub fn select_best_cpu_set(sets: &[CpuSet]) -> Option<u32> {
    if sets.is_empty() {
        return None;
    }
    let best_class = sets.iter().map(|s| s.efficiency_class).max()?;
    let mut candidates: Vec<&CpuSet> =
        sets.iter().filter(|s| s.efficiency_class == best_class).collect();
    candidates.sort_by_key(|s| (s.group, s.logical_index, s.id));

    if candidates.len() > 1 {
        Some(candidates[1].id)
    } else {
        Some(candidates[0].id)
    }
}

/// Enumerate the system's cpu sets. Returns an empty vec if the API fails.
pub fn enumerate_cpu_sets() -> Vec<CpuSet> {
    let mut needed: u32 = 0;

    // SAFETY: passing a null buffer with a zero length is the documented way to
    // query the required size; the call writes only to `needed`.
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &mut needed, Some(GetCurrentProcess()), None);
    }
    if needed == 0 {
        return Vec::new();
    }

    let mut buf = vec![0u8; needed as usize];
    let mut written: u32 = 0;
    // SAFETY: `buf` is `needed` bytes long, which is exactly the size the
    // previous call reported. The pointer is valid for that length and the
    // call writes at most that many bytes.
    let ok = unsafe {
        GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut written,
            Some(GetCurrentProcess()),
            None,
        )
    };
    if !ok.as_bool() || written == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + core::mem::size_of::<u32>() * 2 <= written as usize {
        // SYSTEM_CPU_SET_INFORMATION layout:
        //   0: DWORD Size
        //   4: CPU_SET_INFORMATION_TYPE Type (DWORD; 0 == CpuSetInformation)
        //   8: DWORD Id
        //  12: WORD  Group
        //  14: BYTE  LogicalProcessorIndex
        //  15: BYTE  CoreIndex
        //  16: BYTE  LastLevelCacheIndex
        //  17: BYTE  NumaNodeIndex
        //  18: BYTE  EfficiencyClass
        let size = u32::from_le_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
        if size == 0 || offset + size > written as usize || size < 19 {
            break;
        }
        let ty = u32::from_le_bytes(buf[offset + 4..offset + 8].try_into().unwrap());
        if ty == 0 {
            out.push(CpuSet {
                id: u32::from_le_bytes(buf[offset + 8..offset + 12].try_into().unwrap()),
                group: u16::from_le_bytes(buf[offset + 12..offset + 14].try_into().unwrap()),
                logical_index: buf[offset + 14],
                efficiency_class: buf[offset + 18],
            });
        }
        offset += size;
    }
    out
}

/// Pin the calling thread to the best available cpu set. Returns the chosen id.
///
/// Pinning avoids migration and the cold caches that come with it. On hybrid
/// parts this is also what keeps the hot loop off an E-core.
pub fn pin_current_thread_to_best() -> Option<u32> {
    let sets = enumerate_cpu_sets();
    let chosen = select_best_cpu_set(&sets)?;
    let ids = [chosen];
    // SAFETY: GetCurrentThread returns a pseudo-handle valid on this thread and
    // needing no close. `ids` is a live slice of one u32 read only during the call.
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), &ids) };
    if ok.as_bool() {
        Some(chosen)
    } else {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn set(id: u32, eff: u8) -> CpuSet {
        CpuSet { id, efficiency_class: eff, group: 0, logical_index: id as u8 }
    }

    #[test]
    fn prefers_the_highest_efficiency_class() {
        // Intel hybrid: E-cores report a LOWER EfficiencyClass than P-cores.
        // Landing the hot loop on an E-core is a large, silent regression.
        let sets = vec![set(0, 0), set(1, 0), set(2, 1), set(3, 1)];
        let chosen = select_best_cpu_set(&sets).unwrap();
        assert!(chosen == 2 || chosen == 3, "chose {chosen}, expected a P-core");
    }

    #[test]
    fn homogeneous_cpu_picks_a_stable_non_zero_core() {
        // This dev machine: every set reports the same class.
        let sets: Vec<CpuSet> = (0..12).map(|i| set(i, 0)).collect();
        let a = select_best_cpu_set(&sets).unwrap();
        let b = select_best_cpu_set(&sets).unwrap();
        assert_eq!(a, b, "selection must be deterministic");
        assert_ne!(a, 0, "core 0 handles more interrupt and DPC work; avoid it");
    }

    #[test]
    fn single_core_machine_falls_back_to_core_zero() {
        let sets = vec![set(0, 0)];
        assert_eq!(select_best_cpu_set(&sets), Some(0));
    }

    #[test]
    fn empty_enumeration_selects_nothing() {
        assert_eq!(select_best_cpu_set(&[]), None);
    }

    #[test]
    fn enumeration_returns_at_least_one_cpu_set_on_this_machine() {
        let sets = enumerate_cpu_sets();
        assert!(!sets.is_empty(), "GetSystemCpuSetInformation returned nothing");
    }
}
