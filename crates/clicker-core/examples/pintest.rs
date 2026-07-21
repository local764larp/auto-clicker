//! Diagnose why SetThreadSelectedCpuSets fails.
#[cfg(windows)]
fn main() {
    use clicker_core::affinity::{enumerate_cpu_sets, select_best_cpu_set};
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadSelectedCpuSets};

    let sets = enumerate_cpu_sets();
    let chosen = select_best_cpu_set(&sets).unwrap();
    println!("chosen id = {chosen}");

    let ids = [chosen];
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), &ids) };
    let err = unsafe { GetLastError() }.0;
    println!("SetThreadSelectedCpuSets -> {:?}  GetLastError={}", ok.as_bool(), err);
}
#[cfg(not(windows))]
fn main() {}
