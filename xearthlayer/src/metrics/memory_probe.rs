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
    /// Anonymous memory swapped out to disk, in bytes. `None` on platforms
    /// where it is not read.
    ///
    /// This is the field that matters most for diagnosing issue #209: in the
    /// OOM flight, 54.7 GB of 64 GB anonymous memory was swapped out while
    /// `rss_bytes` alone read a healthy 9.3 GB. A trace that only logs
    /// `rss_bytes` cannot see a process swapping itself to death.
    pub swap_bytes: Option<u64>,
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

/// Serializes the first call to `memory_stats()` to work around a race in memory-stats 1.2.0.
///
/// memory-stats uses an atomic CAS to guard init on Linux. A concurrent loser thread may read
/// the uninitialized state before the winner has written it, causing PAGE_SIZE to remain 0 and
/// both physical_mem and virtual_mem to come out as 0. Calling once here ensures the static
/// init completes before any concurrent use in the test suite or daemon.
static MEMORY_STATS_INIT: std::sync::Once = std::sync::Once::new();

impl ProcessMemoryProbe {
    /// Creates a new probe.
    pub fn new() -> Self {
        Self
    }

    /// Reads OS thread count and swapped-out anonymous memory in one pass
    /// over `/proc/self/status`.
    ///
    /// Linux-only and best-effort: `memory-stats` does not expose either
    /// value, and hand-rolling mach FFI for macOS is not worth the risk on a
    /// blocking CI platform. Thread count is tracked because tokio's blocking
    /// pool (default 512 threads) is implicated in issue #209; swap is
    /// tracked because it is the field that actually caught #209 — see
    /// `MemorySample::swap_bytes`.
    ///
    /// Both fields are read from the same file read so the file is only
    /// opened once per sample.
    #[cfg(target_os = "linux")]
    fn linux_status_fields() -> (Option<u64>, Option<u64>) {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return (None, None);
        };

        let mut threads = None;
        let mut swap_bytes = None;

        for line in status.lines() {
            if let Some(value) = line.strip_prefix("Threads:") {
                threads = value.trim().parse().ok();
            } else if let Some(value) = line.strip_prefix("VmSwap:") {
                // Format is like "      0 kB" - strip the "kB" suffix, then
                // the value is already in kB per the /proc/self/status contract.
                swap_bytes = value
                    .trim()
                    .strip_suffix("kB")
                    .and_then(|kb| kb.trim().parse::<u64>().ok())
                    .map(|kb| kb * 1024);
            }
        }

        (threads, swap_bytes)
    }

    #[cfg(not(target_os = "linux"))]
    fn linux_status_fields() -> (Option<u64>, Option<u64>) {
        (None, None)
    }
}

impl MemoryProbe for ProcessMemoryProbe {
    fn sample(&self) -> Option<MemorySample> {
        // Serialize the first call to memory_stats to ensure upstream initialization completes
        // before concurrent use (see MEMORY_STATS_INIT doc comment).
        MEMORY_STATS_INIT.call_once(|| {
            let _ = memory_stats::memory_stats();
        });

        let stats = memory_stats::memory_stats()?;
        let (threads, swap_bytes) = Self::linux_status_fields();
        Some(MemorySample {
            rss_bytes: stats.physical_mem as u64,
            vm_bytes: stats.virtual_mem as u64,
            threads,
            swap_bytes,
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
    /// Creates a probe returning the given byte counts, 42 threads, and zero swap.
    pub(crate) fn new(rss_bytes: u64, vm_bytes: u64) -> Self {
        Self {
            sample: Some(MemorySample {
                rss_bytes,
                vm_bytes,
                threads: Some(42),
                swap_bytes: Some(0),
            }),
        }
    }

    /// Overrides the configured swap byte count.
    ///
    /// No-op if the probe was built with [`Self::unavailable`].
    pub(crate) fn with_swap_bytes(mut self, swap_bytes: u64) -> Self {
        if let Some(sample) = self.sample.as_mut() {
            sample.swap_bytes = Some(swap_bytes);
        }
        self
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
        assert_eq!(sample.swap_bytes, Some(0));
    }

    #[test]
    fn static_probe_swap_bytes_can_be_overridden() {
        let probe = StaticMemoryProbe::new(1024, 2048).with_swap_bytes(9_999);
        let sample = probe.sample().unwrap();
        assert_eq!(sample.swap_bytes, Some(9_999));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_reports_swap_bytes() {
        let sample = ProcessMemoryProbe::new().sample().unwrap();
        assert!(
            sample.swap_bytes.is_some(),
            "linux should always expose VmSwap, even if the value is 0"
        );
    }
}
