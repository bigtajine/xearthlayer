//! Process memory sampling.
//!
//! Provides a mockable abstraction over reading the current process's memory
//! footprint. The production implementation is backed by the `memory-stats`
//! crate, which supports Linux, macOS and Windows; on Unix its only dependency
//! is `libc`, already in this crate's tree.

/// A point-in-time sample of the process's memory footprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySample {
    /// Resident set size in bytes (physical memory currently held).
    pub rss_bytes: u64,
    /// Virtual memory size in bytes (address space reserved).
    pub vm_bytes: u64,
    /// OS thread count. `None` on platforms where it is not read.
    pub threads: Option<u64>,
}

/// Reads the current process's memory footprint.
///
/// Implemented as a trait so the metrics daemon can be tested without reading
/// real process memory.
pub trait MemoryProbe: Send + Sync {
    /// Returns the current sample, or `None` if the platform cannot supply one.
    fn sample(&self) -> Option<MemorySample>;
}

/// Production probe backed by the `memory-stats` crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessMemoryProbe;

impl ProcessMemoryProbe {
    /// Creates a new probe.
    pub fn new() -> Self {
        Self
    }

    /// Reads the OS thread count.
    ///
    /// Linux-only and best-effort: `memory-stats` does not expose thread count,
    /// and hand-rolling mach FFI for macOS is not worth the risk on a blocking
    /// CI platform. Thread count is tracked because tokio's blocking pool
    /// (default 512 threads) is implicated in issue #209.
    #[cfg(target_os = "linux")]
    fn thread_count() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("Threads:"))
            .and_then(|value| value.trim().parse().ok())
    }

    #[cfg(not(target_os = "linux"))]
    fn thread_count() -> Option<u64> {
        None
    }
}

impl MemoryProbe for ProcessMemoryProbe {
    fn sample(&self) -> Option<MemorySample> {
        let stats = memory_stats::memory_stats()?;
        Some(MemorySample {
            rss_bytes: stats.physical_mem as u64,
            vm_bytes: stats.virtual_mem as u64,
            threads: Self::thread_count(),
        })
    }
}

/// Logs glibc allocator overrides when any are set.
///
/// These change how freed memory is returned to the OS, so a trace gathered
/// with them set is not comparable to one gathered without. Recording them
/// prevents silently comparing incomparable runs.
pub fn log_allocator_environment() {
    let overrides: Vec<String> = [
        "MALLOC_ARENA_MAX",
        "MALLOC_MMAP_THRESHOLD_",
        "MALLOC_TRIM_THRESHOLD_",
    ]
    .iter()
    .filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| format!("{}={}", key, value))
    })
    .collect();

    if !overrides.is_empty() {
        tracing::info!(
            allocator_env = %overrides.join(" "),
            "Allocator environment overrides active"
        );
    }
}

/// Test double returning fixed values.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct StaticMemoryProbe {
    sample: Option<MemorySample>,
}

#[cfg(test)]
impl StaticMemoryProbe {
    /// Creates a probe returning the given byte counts and 42 threads.
    pub(crate) fn new(rss_bytes: u64, vm_bytes: u64) -> Self {
        Self {
            sample: Some(MemorySample {
                rss_bytes,
                vm_bytes,
                threads: Some(42),
            }),
        }
    }

    /// Creates a probe that always fails to sample.
    pub(crate) fn unavailable() -> Self {
        Self { sample: None }
    }
}

#[cfg(test)]
impl MemoryProbe for StaticMemoryProbe {
    fn sample(&self) -> Option<MemorySample> {
        self.sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_probe_reports_nonzero_memory() {
        let probe = ProcessMemoryProbe::new();
        let sample = probe
            .sample()
            .expect("probe must work on supported platforms");
        assert!(sample.rss_bytes > 0, "rss_bytes should be positive");
        assert!(sample.vm_bytes > 0, "vm_bytes should be positive");
    }

    #[test]
    fn static_probe_returns_configured_values() {
        let probe = StaticMemoryProbe::new(1024, 2048);
        let sample = probe.sample().unwrap();
        assert_eq!(sample.rss_bytes, 1024);
        assert_eq!(sample.vm_bytes, 2048);
    }

    #[test]
    fn unavailable_probe_returns_none() {
        assert!(StaticMemoryProbe::unavailable().sample().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_reports_thread_count() {
        let sample = ProcessMemoryProbe::new().sample().unwrap();
        assert!(
            sample.threads.unwrap_or(0) >= 1,
            "linux should report >= 1 thread"
        );
    }
}
