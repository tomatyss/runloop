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

pub fn read_stats(tid: Option<u32>) -> Result<AgentStats, Error> {
    match tid {
        Some(t) => read_thread_stats(t),
        None => Ok(AgentStats::none()),
    }
}

#[cfg(feature = "procfs")]
fn read_thread_stats(tid: u32) -> Result<AgentStats, Error> {
    use procfs::process::Process;

    let process = Process::myself().map_err(|_| Error::StatsUnavailable)?;
    let task = process
        .task_by_tid(tid as i32)
        .map_err(|_| Error::StatsUnavailable)?;
    let stat = task.stat().map_err(|_| Error::StatsUnavailable)?;
    let ticks_per_second = procfs::ticks_per_second().map_err(|_| Error::StatsUnavailable)? as u64;
    let total_ticks = stat.utime + stat.stime;
    let cpu_total_ms = if ticks_per_second == 0 {
        None
    } else {
        Some((total_ticks * 1_000) / ticks_per_second)
    };
    let rss_bytes = stat.rss_bytes().map(|b| b.get()).ok();
    Ok(AgentStats {
        rss_bytes,
        cpu_total_ms,
    })
}

#[cfg(not(feature = "procfs"))]
fn read_thread_stats(_tid: u32) -> Result<AgentStats, Error> {
    Ok(AgentStats::none())
}
