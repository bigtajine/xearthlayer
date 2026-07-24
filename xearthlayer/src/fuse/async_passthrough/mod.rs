//! Shared types for FUSE filesystem DDS generation.
//!
//! This module provides types used by the fuse3 passthrough filesystem:
//! - [`types`] - Request/response types for DDS generation
//! - [`inode`] - Inode allocation and management

// Shared types used by fuse3
pub mod inode;
mod types;

// Re-export shared types for use by fuse3 and public API
pub use types::{DdsHandler, DdsRequest, DdsResponse};
