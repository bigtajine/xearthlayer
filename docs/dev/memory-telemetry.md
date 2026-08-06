# Memory Telemetry

**Status**: Implemented
**Added**: v0.4.7
**Issue**: [#209](https://github.com/samsoir/xearthlayer/issues/209)

## Purpose

XEarthLayer emits one structured memory sample per minute into the normal log
file for the entire life of the process. The sample is always on, needs no
flags, no environment variables, and no profiler, and it costs one read of
`/proc/self/smaps` plus one `tracing::info!` every 60 seconds.

It exists because of issue #209. A 12-hour flight ended with the kernel
OOM-killing the process at 64 GB of anonymous memory — 9.3 GB resident plus
54.7 GB swapped — against a configured 4 GB memory cache. There was no
application error and no panic: the last line in the log is the instant of the
kill. The profiler the project already had — heaptrack — writes its output on
exit, so a `SIGKILL` leaves nothing behind. The trace is deliberately coarse,
but it is already on disk when the kernel fires.

Three candidate causes for #209 are still live, and the sample line is designed
so that a single unattended flight discriminates between them:

1. **Unbounded fire-and-forget cache-write backlog.** `BuildAndCacheDdsTask`
   issues two ungated `tokio::spawn` calls per completed tile — one memory cache
   write, one DDS disk cache write — each holding its own clone of the encoded
   tile (~11.2 MB for BC1 with a full mipmap chain). The disk write reaches
   storage through `tokio::fs`, which queues on the tokio blocking pool. Neither
   spawn is gated by the executor's `max_concurrent_jobs` or by any
   `ResourcePool` permit, so if the blocking pool saturates the queue and its
   buffers grow without limit.
2. **glibc arena retention.** An 11.2 MB buffer sits just below glibc's adaptive
   `mmap` threshold ceiling of 32 MB. Once the threshold has adapted upward past
   the buffer size, those allocations are served from per-thread arenas rather
   than by `mmap`, and freeing them returns the memory to the arena free list,
   not to the OS.
3. **Chunk LRU index growth.** The chunk disk cache index held roughly
   12 million entries during the OOM flight.

## The sample line

```
2026-08-06 06:20:33Z  INFO Memory sample uptime_s=60 rss_mb=873 vm_mb=1459 threads=71 tiles_done=0 encodes_active=0 chunks_ok=0 chunks_failed=0 mem_cache_mb=0 dds_disk_mb=158268 chunk_disk_mb=55301 gc_evicted_mb=0 chunk_index_entries=3803083 disk_writes_active=0
```

Fourteen fields, in emission order. Every `_mb` value is truncating integer
division by 1 MiB (1 048 576 bytes), so a value of `0` means "under one
mebibyte", not necessarily "nothing".

| Field | Source | Meaning |
|-------|--------|---------|
| `uptime_s` | `AggregatedState::uptime()` | Whole seconds since the metrics daemon started, which is effectively service start. Use it as the x-axis. |
| `rss_mb` | `MemoryProbe` → sum of `Rss:` in `/proc/self/smaps` | Physical pages currently held. **Excludes anything the kernel has swapped out.** |
| `vm_mb` | `MemoryProbe` → sum of `Size:` in `/proc/self/smaps` | Total mapped address space. Counts anonymous mappings whether resident or paged out. |
| `threads` | `/proc/self/status` `Threads:` | OS thread count. `0` means **not readable on this platform**, not "no threads" — see below. |
| `tiles_done` | `state.encodes_completed` | Cumulative DDS encodes completed. The load counter: divide by `uptime_s` for tiles/second. |
| `encodes_active` | `state.encodes_active` | Encodes in flight right now. Each one holds a 4096×4096 RGBA source image (64 MiB) plus its output buffer. |
| `chunks_ok` | `state.chunks_downloaded` | Cumulative successful chunk downloads. |
| `chunks_failed` | `state.chunks_failed` | Cumulative failed chunk downloads. A rising count means tiles are being served but not persisted (see issue #180). |
| `mem_cache_mb` | `state.memory_cache_size_bytes` | Current moka memory cache size. Compare against `cache.memory_size`. |
| `dds_disk_mb` | `state.dds_disk_cache_size_bytes` | Current DDS disk tier size, from that tier's LRU index. |
| `chunk_disk_mb` | `state.chunk_disk_cache_size_bytes` | Current chunk disk tier size, from that tier's LRU index. |
| `gc_evicted_mb` | `state.disk_bytes_evicted` | Cumulative bytes freed by the disk GC daemons, **summed across both disk tiers**. |
| `chunk_index_entries` | `state.chunk_index_entries` | Live entries in the **chunk** tier's `LruIndex`. The DDS tier does not report this. |
| `disk_writes_active` | `state.disk_writes_active` | Fire-and-forget disk cache writes currently in flight, counting both chunk writes from `DownloadChunksTask` and DDS tile writes from `BuildAndCacheDdsTask`. |

### `threads=0` means unreadable, not zero

Thread count is read from `/proc/self/status`, which only exists on Linux.
`memory-stats` does not expose a thread count, and hand-rolling mach FFI for
macOS was judged not worth the risk on a CI-blocking platform, so
`ProcessMemoryProbe::thread_count()` returns `None` everywhere except Linux and
the emit line renders `None` as `0`. A macOS trace will show `threads=0` on
every line; that tells you nothing about the process.

### `rss_mb` and `vm_mb` are not interchangeable

`rss_mb` counts resident pages only. On a machine that is swapping — which is
exactly the machine that is about to be OOM-killed — a growing process can show
a **flat or falling** `rss_mb` while the kernel quietly moves its pages to swap.
In the #209 flight only 9.3 GB of the 64 GB was resident at the moment of the
kill.

`vm_mb` counts the whole mapping regardless of residency, so it keeps rising.
The gap between the two is lazily-reserved-but-never-touched address space plus
anything paged out. A widening `vm_mb - rss_mb` gap on a system with swap
enabled is the signal that matters; do not read `rss_mb` alone.

### The allocator environment line

`log_allocator_environment()` runs once at startup from `CliRunner::log_startup`
and emits a line only when at least one of `MALLOC_ARENA_MAX`,
`MALLOC_MMAP_THRESHOLD_` or `MALLOC_TRIM_THRESHOLD_` is set:

```
Allocator environment overrides active allocator_env=MALLOC_MMAP_THRESHOLD_=1048576 MALLOC_ARENA_MAX=2
```

These variables change whether freed memory goes back to the OS, so a trace
gathered with them set is not comparable to one gathered without. **The absence
of this line is itself information**: it means the run used stock glibc
behaviour. Always check for it before comparing two traces.

## Reading a trace

Plot `rss_mb`, `vm_mb`, `disk_writes_active` and `chunk_index_entries` against
`uptime_s`. The shape of the first against the other three is what
discriminates the three candidates.

| `rss_mb` / `vm_mb` | `disk_writes_active` | `chunk_index_entries` | Reading |
|--------------------|----------------------|-----------------------|---------|
| climbing | climbing | any | **Candidate 1** — the fire-and-forget cache-write backlog in `tasks/build_and_cache_dds.rs`. Each queued write pins ~11.2 MB, and the paired memory-cache write pins another ~11.2 MB that this gauge does not count. |
| climbing | flat | climbing | **Candidate 3** — chunk LRU index growth. Cross-check that `chunk_disk_mb` is at its configured ceiling and `gc_evicted_mb` is rising: an index that grows while the tier size is pinned means entries are accumulating faster than GC removes them. |
| climbing | flat | flat | **Candidate 2** — allocator retention. Nothing in the process is holding more logical data, but the resident footprint grows anyway. Confirm by re-running with `MALLOC_MMAP_THRESHOLD_=1048576`; if growth stops, glibc was hoarding freed buffers in per-thread arenas. |

Supporting reads:

- **`disk_writes_active` is a lower bound.** The counter is incremented by the
  first statement *inside* each spawned write task, so a write that has been
  spawned but not yet polled is invisible. If the tokio worker threads are
  themselves starved, the real backlog is larger than the number printed.
- **`encodes_active` × 64 MiB is the floor of what encoding costs.** Each
  in-flight encode holds a 4096×4096 RGBA source image (64 MiB) plus its output
  buffer. The number should track the CPU resource pool capacity; if it runs
  materially above that, CPU admission control is not bounding the pipeline and
  the peak is unbounded with it.
- **`threads`** climbing toward 512 points at the tokio blocking pool (default
  512 threads) filling up, which is the mechanism behind candidate 1.
- **`tiles_done` per hour** is the load normaliser. Two traces at wildly
  different tile rates are not telling you about memory, they are telling you
  about workload.
- **`chunks_failed`** rising alongside `tiles_done` means tiles are being served
  to X-Plane but never persisted, so the same tiles will be regenerated
  repeatedly. That inflates apparent load without inflating cache size.

## Comparing two runs — read this first

**Matching load between two runs is not sufficient to make them comparable.**

The worked example is the 2026-08-05 retest of issue #209. It was run with
`MALLOC_MMAP_THRESHOLD_=1048576 MALLOC_ARENA_MAX=2` and held at 5.3 GB, against
an OOM kill at 64 GB. It also matched the OOM flight to within 1% on tiles
generated per hour. It looked like a clean confirmation of candidate 2. It was
not:

| | OOM flight | 2026-08-05 retest |
|---|---|---|
| Peak anonymous memory | 64 GB (killed) | 5.3 GB |
| Tiles generated per hour | baseline | within 1% of baseline |
| Allocator overrides | none | `MALLOC_MMAP_THRESHOLD_=1048576`, `MALLOC_ARENA_MAX=2` |
| Disk tier occupancy (DDS / chunks) | 89% / 85% | empty throughout |
| GC evictions over the run | ~24 million files | zero |
| Chunk index entries | ~12 million | small and growing from zero |

Two major variables moved between the runs, not one. The retest changed the
allocator *and* removed all cache pressure. A run that never evicts anything
never exercises the GC path, never grows the LRU index to steady state, and
never sustains the disk write queue depth that a full cache produces. The
experiment therefore discriminated nothing: the 5.3 GB ceiling is equally
consistent with "the allocator fix worked" and with "an empty cache produces a
fundamentally different workload".

Before comparing two traces, check that all of the following are in the same
regime:

1. **`dds_disk_mb` and `chunk_disk_mb`** — are both runs at their configured
   ceilings, or is one starting from empty? A cold cache is a different program.
2. **`gc_evicted_mb`** — is GC actually running in both? Zero evictions means
   the eviction path was never exercised.
3. **`chunk_index_entries`** — is the index at steady state in both, or growing
   from zero in one?
4. **The allocator environment line** — present in both, absent in both, or
   present in only one?
5. **`tiles_done / uptime_s`** — comparable tile rates.

Only then is a difference in `rss_mb` attributable to the change under test.
Change one variable per flight. To test candidate 2 properly, the allocator
override must be flown against a cache that is already full and evicting.

## Submitting a trace

The trace is plain text in the normal log file, `~/.xearthlayer/xearthlayer.log`
by default (configurable via `logging.file`; see
[Configuration](../configuration.md)). Attach the whole file to the GitHub
issue — the memory samples are hard to interpret without the surrounding startup
lines: the version banner, the resolved cache sizes, and the allocator
environment line if one was emitted.

> **The log file is truncated on every start.** `init_logging_full` clears the
> file before opening it, so relaunching XEarthLayer destroys the previous
> flight's trace. Copy it aside *before* the next launch:
>
> ```bash
> cp ~/.xearthlayer/xearthlayer.log ~/flight-$(date +%Y%m%d-%H%M).log
> ```

To extract just the samples for plotting:

```bash
grep "Memory sample" ~/.xearthlayer/xearthlayer.log
```

A 12-hour flight produces 720 sample lines.

## Architecture

```
metrics/memory_probe.rs                    metrics/daemon.rs
───────────────────────                    ─────────────────
MemoryProbe (trait)          injected      MetricsDaemon
  fn sample() -> Option<     ───────────▶    with_memory_probe(rx, probe)
      MemorySample>                          run() → select! { … }
                                               MEMORY_SAMPLE_INTERVAL tick
ProcessMemoryProbe                             → log_memory_sample()
  memory-stats + /proc                              │
StaticMemoryProbe (test)                            ▼
                                             tracing::info!("Memory sample", …)
```

### `MemoryProbe`

```rust
pub trait MemoryProbe: Send + Sync {
    fn sample(&self) -> Option<MemorySample>;
}
```

`MemorySample` carries `rss_bytes`, `vm_bytes` and `threads: Option<u64>`.
Returning `Option` rather than a `Result` is deliberate: a platform that cannot
supply a reading is not an error condition, it just means no sample line.

`ProcessMemoryProbe` is the production implementation. It delegates to the
`memory-stats` crate for the byte counts (Linux, macOS and Windows; on Unix its
only dependency is `libc`, already in the tree) and reads thread count itself
from `/proc/self/status` on Linux only.

### The `memory-stats` initialisation workaround

`ProcessMemoryProbe::sample()` funnels the first call through a
`std::sync::Once`:

```rust
static MEMORY_STATS_INIT: std::sync::Once = std::sync::Once::new();
```

This works around a race in memory-stats 1.2.0. Its Linux path guards
initialisation with an atomic compare-exchange on `SMAPS_CHECKED`. A thread that
loses that CAS can read the still-default `SMAPS_EXIST` (`false`) before the
winner has stored the real value, fall through to the `/proc/self/statm`
fallback, and multiply the page counts by a `PAGE_SIZE` that is still `0` —
producing a silent, non-erroring reading of `physical_mem = 0` and
`virtual_mem = 0`. Serialising the first call ensures upstream initialisation
has completed before any concurrent use. Without it, the first sample of a run
can be a plausible-looking `rss_mb=0 vm_mb=0`.

### Injection and sampling cadence

`MetricsDaemon::new` wires in `ProcessMemoryProbe`; `with_memory_probe` takes an
`Arc<dyn MemoryProbe>` so tests can substitute `StaticMemoryProbe` and assert on
the emitted event without reading real process memory. The daemon owns a second
`tokio::time::interval` alongside the 100 ms time-series sampler:

```rust
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
```

Sixty seconds is fixed and **deliberately not configurable**. Traces are pooled
across users and machines, and a configurable cadence would be one more variable
to reconcile before two traces could be compared — precisely the failure mode
described above. Both intervals use `MissedTickBehavior::Skip` so a stalled
daemon does not emit a burst of catch-up samples.

If the probe returns `None`, the daemon logs one warning
(`"Memory probe unavailable; memory samples disabled"`), latches
`memory_probe_failed`, and emits nothing further. The warning is not repeated.

Because `MetricsSystem::new` is constructed unconditionally in
`XEarthLayerService::start`, memory sampling is active for every run, including
TUI mode.

### Adding a platform implementation

To add thread count (or any other field) for a new platform:

1. Add a `#[cfg(target_os = "…")]` branch to `ProcessMemoryProbe::thread_count`.
   The existing `#[cfg(not(target_os = "linux"))]` fallback returns `None`, so
   nothing breaks if you add a platform and forget a branch — the field just
   renders as `0`.
2. Extend `MemorySample` if the platform can supply something the others cannot.
   Any new field must be `Option`-typed with an emit-time default, so a trace
   from one platform stays diffable against a trace from another.
3. Add the field to the `tracing::info!` call in `log_memory_sample` **at the
   end** of the field list. Existing field positions are what makes traces from
   different builds comparable by eye.
4. Update the field table in this document and the assertion list in
   `memory_sample_line_carries_every_field_from_its_correct_source`, which seeds
   every field to a distinct value specifically so a swapped or dropped source
   cannot pass.

## Relationship to heaptrack

The two tools answer different questions and neither replaces the other.

[Memory Profiling](memory-profiling.md) with heaptrack gives allocation-level
attribution: which call site allocated what, ranked by contribution to peak
heap, with full backtraces. That is what you want when you already know a
workload reproduces the growth and you need to know *which line of code* is
responsible. But heaptrack writes its summary on exit — a crash or `kill -9`
loses data — so it cannot capture an OOM kill, which is by definition a
`SIGKILL`. It also adds a 2-3× slowdown on allocation-heavy code, which makes it
impractical for a 12-hour flight.

Memory telemetry gives no attribution at all. It tells you the footprint grew,
roughly how fast, and which of a small number of subsystem gauges moved with it.
Its advantages are that it costs nothing, runs unattended for the whole flight,
and is durably on disk one minute at a time, so it survives the kill that
heaptrack cannot.

| | Memory telemetry | heaptrack |
|---|---|---|
| Granularity | Process-level gauges | Per-call-site allocations |
| Overhead | Negligible | 2-3× on allocation-heavy paths |
| Survives `kill -9` | Yes | No |
| Practical duration | Unbounded | Minutes |
| Usable by other users | Yes — always on, just attach the log | No — requires a profiler build and a local run |

Use the trace for long unattended flights and for data reported by other users.
Use heaptrack for local attribution once the trace has narrowed the search to a
reproducible workload.

## References

- [Memory Profiling](memory-profiling.md) — heaptrack guide
- [Cache Service Design](cache-service-design.md) — `CacheLayer`, GC daemons, the tiers behind `mem_cache_mb` / `dds_disk_mb` / `chunk_disk_mb`
- [Job Executor Design](job-executor-design.md) — resource pools and `max_concurrent_jobs`, which the fire-and-forget cache writes bypass
- [Configuration](../configuration.md) — `logging.file`, `cache.memory_size`, `cache.disk_size`
- Issue [#209](https://github.com/samsoir/xearthlayer/issues/209) — the OOM kill this telemetry was added to diagnose
- Issue [#180](https://github.com/samsoir/xearthlayer/issues/180) — why `chunks_failed` suppresses cache writes
