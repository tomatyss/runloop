use crate::error::Error;

/// Snapshot of per-agent resource usage.
#[derive(Debug, Clone, Default)]
pub struct AgentStats {
    pub rss_bytes: Option<u64>,
    pub cpu_total_ms: Option<u64>,
}

impl AgentStats {
    pub fn none() -> Self {
        Self::default()
    }
}

/// Read per-agent statistics, preferring Linux `/proc` sampling when available.
pub fn read_stats(tid: Option<u32>) -> Result<AgentStats, Error> {
    cfg_if::cfg_if! {
        if #[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))] {
            read_stats_linux(tid)
        } else {
            let _ = tid;
            read_stats_portable()
        }
    }
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
fn read_stats_linux(tid: Option<u32>) -> Result<AgentStats, Error> {
    let ticks_per_second = procfs::ticks_per_second().into_stats_value()?.max(1);
    let page_size = procfs::page_size().into_stats_value()?.max(1);

    if let Some(tid) = tid
        && let Ok(stats) = read_thread_stats_procfs(tid, ticks_per_second, page_size)
    {
        return Ok(stats);
    }

    read_process_stats_procfs(ticks_per_second, page_size)
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
fn read_thread_stats_procfs(
    tid: u32,
    ticks_per_second: u64,
    page_size: u64,
) -> Result<AgentStats, Error> {
    use procfs::process::Process;

    let process = Process::myself().map_err(|_| Error::StatsUnavailable)?;
    let task = process
        .task_from_tid(tid as i32)
        .map_err(|_| Error::StatsUnavailable)?;
    let stat = task.stat().map_err(|_| Error::StatsUnavailable)?;
    let total_ticks = stat.utime.saturating_add(stat.stime);
    let cpu_total_ms = ticks_to_millis(total_ticks, ticks_per_second);
    let rss_pages = stat.rss;
    let rss_bytes = rss_pages.saturating_mul(page_size);

    Ok(AgentStats {
        rss_bytes: Some(rss_bytes),
        cpu_total_ms: Some(cpu_total_ms),
    })
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
fn read_process_stats_procfs(ticks_per_second: u64, page_size: u64) -> Result<AgentStats, Error> {
    use procfs::process::Process;

    let process = Process::myself().map_err(|_| Error::StatsUnavailable)?;
    let stat = process.stat().map_err(|_| Error::StatsUnavailable)?;
    let total_ticks = stat.utime.saturating_add(stat.stime);
    let cpu_total_ms = ticks_to_millis(total_ticks, ticks_per_second);
    let rss_pages = stat.rss;
    let rss_bytes = rss_pages.saturating_mul(page_size);

    Ok(AgentStats {
        rss_bytes: Some(rss_bytes),
        cpu_total_ms: Some(cpu_total_ms),
    })
}

#[cfg(not(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs"))))]
fn read_stats_portable() -> Result<AgentStats, Error> {
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

    let pid = Pid::from_u32(std::process::id());
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::everything()),
    );
    if !system.refresh_process(pid) {
        return Err(Error::StatsUnavailable);
    }
    let process = system.process(pid).ok_or(Error::StatsUnavailable)?;
    Ok(AgentStats {
        rss_bytes: Some(process.memory() * 1024),
        cpu_total_ms: None,
    })
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
fn ticks_to_millis(total_ticks: u64, ticks_per_second: u64) -> u64 {
    if ticks_per_second == 0 {
        return 0;
    }
    let numerator = (total_ticks as u128).saturating_mul(1_000);
    let divisor = ticks_per_second as u128;
    let millis = numerator / divisor;
    millis.min(u128::from(u64::MAX)) as u64
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
trait ProcfsValueExt {
    fn into_stats_value(self) -> Result<u64, Error>;
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
impl ProcfsValueExt for u64 {
    fn into_stats_value(self) -> Result<u64, Error> {
        Ok(self)
    }
}

#[cfg(all(target_os = "linux", feature = "procfs", not(feature = "no-procfs")))]
impl ProcfsValueExt for Result<u64, procfs::ProcError> {
    fn into_stats_value(self) -> Result<u64, Error> {
        self.map_err(|_| Error::StatsUnavailable)
    }
}
