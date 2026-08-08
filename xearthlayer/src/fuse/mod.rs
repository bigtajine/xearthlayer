//! FUSE filesystem for on-demand DDS texture generation.
//!
//! Provides a virtual filesystem that intercepts X-Plane texture reads
//! and generates satellite imagery DDS files on demand.
//!
//! # Implementation
//!
//! Uses [`Fuse3PassthroughFS`] - an async multi-threaded passthrough filesystem
//! that overlays existing scenery directories while generating DDS textures on-demand.

// Internal support modules: shared types + inode management (used by fuse3)
pub(crate) mod support;

mod coalesce;
mod filename;
#[cfg(target_os = "linux")]
pub mod fuse3;
mod placeholder;
#[cfg(target_os = "windows")]
pub mod windows;

// Re-export types for public API
pub use coalesce::{CoalesceResult, CoalescedResult, CoalescerStats, RequestCoalescer};
pub use filename::{parse_dds_filename, DdsFilename, ParseError};
#[cfg(target_os = "linux")]
pub use fuse3::{
    Fuse3Error as MountError, Fuse3OrthoUnionFS as OrthoUnionFS,
    Fuse3PassthroughFS as PassthroughFS, Fuse3Result as MountResult, MountHandle,
    SpawnedMountHandle,
};
#[cfg(target_os = "windows")]
pub use windows::{
    MountError, MountHandle, MountResult, OrthoUnionFS, PassthroughFS, SpawnedMountHandle,
};
pub use placeholder::{
    generate_default_placeholder, generate_magenta_placeholder, get_default_placeholder,
    init_placeholder_cache, validate_dds_or_placeholder, EXPECTED_DDS_SIZE,
};
pub use support::{DdsHandler, DdsRequest, DdsResponse};
