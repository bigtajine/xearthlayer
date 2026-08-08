//! Windows virtual filesystem stub.
//!
//! Mirrors the public API of the Linux fuse3 implementation
//! ([`PassthroughFS`], [`OrthoUnionFS`], [`MountHandle`], [`SpawnedMountHandle`])
//! so the rest of the codebase (service/manager layers) compiles unchanged
//! on Windows. Mounting is not implemented yet — the real driver work is a
//! WinFsp/Dokan-backed filesystem replacing these stubs.
//!
//! ponytail: stub only, upgrade path is implementing the Dokan callbacks in
//! this module using the same builder API so call sites need no changes.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::mpsc;

use crate::executor::DdsClient;
use crate::geo_index::GeoIndex;
use crate::ortho_union::OrthoUnionIndex;
use crate::prefetch::{DdsAccessEvent as CoreDdsAccessEvent, TileRequestCallback};
use crate::scene_tracker::FuseAccessEvent;

pub type MountResult<T> = Result<T, MountError>;

#[derive(Debug, Error)]
pub enum MountError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Mount failed: {0}")]
    MountFailed(String),
    #[error("Invalid path: {0}")]
    InvalidPath(String),
}

fn not_implemented() -> MountError {
    MountError::MountFailed(
        "Windows virtual filesystem is not implemented yet (tracked follow-up: WinFsp/Dokan port)"
            .to_string(),
    )
}

pub struct MountHandle;

impl Future for MountHandle {
    type Output = io::Result<()>;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Ok(()))
    }
}

pub struct SpawnedMountHandle;

impl SpawnedMountHandle {
    pub async fn unmount(self) -> io::Result<()> {
        Ok(())
    }
}

pub struct PassthroughFS {
    #[allow(dead_code)]
    source_dir: PathBuf,
    #[allow(dead_code)]
    dds_client: Arc<dyn DdsClient>,
    #[allow(dead_code)]
    expected_dds_size: usize,
    #[allow(dead_code)]
    timeout: Duration,
    #[allow(dead_code)]
    tile_request_callback: Option<TileRequestCallback>,
}

impl PassthroughFS {
    pub fn new(source_dir: PathBuf, dds_client: Arc<dyn DdsClient>, expected_dds_size: usize) -> Self {
        Self {
            source_dir,
            dds_client,
            expected_dds_size,
            timeout: Duration::from_secs(30),
            tile_request_callback: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_tile_request_callback(mut self, callback: TileRequestCallback) -> Self {
        self.tile_request_callback = Some(callback);
        self
    }

    pub async fn mount(self, _mountpoint: &str) -> MountResult<MountHandle> {
        Err(not_implemented())
    }

    pub async fn mount_spawned(self, _mountpoint: &str) -> MountResult<SpawnedMountHandle> {
        Err(not_implemented())
    }
}

pub struct OrthoUnionFS {
    #[allow(dead_code)]
    index: OrthoUnionIndex,
    #[allow(dead_code)]
    dds_client: Arc<dyn DdsClient>,
    #[allow(dead_code)]
    expected_dds_size: usize,
    #[allow(dead_code)]
    geo_index: Option<Arc<GeoIndex>>,
    #[allow(dead_code)]
    dds_access_tx: Option<mpsc::UnboundedSender<CoreDdsAccessEvent>>,
    #[allow(dead_code)]
    scene_tracker_tx: Option<mpsc::UnboundedSender<FuseAccessEvent>>,
    #[allow(dead_code)]
    fuse_max_background: Option<u16>,
    #[allow(dead_code)]
    fuse_congestion_threshold: Option<u16>,
}

impl OrthoUnionFS {
    pub fn new(index: OrthoUnionIndex, dds_client: Arc<dyn DdsClient>, expected_dds_size: usize) -> Self {
        Self {
            index,
            dds_client,
            expected_dds_size,
            geo_index: None,
            dds_access_tx: None,
            scene_tracker_tx: None,
            fuse_max_background: None,
            fuse_congestion_threshold: None,
        }
    }

    pub fn with_geo_index(mut self, geo_index: Arc<GeoIndex>) -> Self {
        self.geo_index = Some(geo_index);
        self
    }

    pub fn with_dds_access_channel(mut self, tx: mpsc::UnboundedSender<CoreDdsAccessEvent>) -> Self {
        self.dds_access_tx = Some(tx);
        self
    }

    pub fn with_scene_tracker_channel(mut self, tx: mpsc::UnboundedSender<FuseAccessEvent>) -> Self {
        self.scene_tracker_tx = Some(tx);
        self
    }

    pub fn with_metrics(self, _metrics: crate::metrics::MetricsClient) -> Self {
        self
    }

    pub fn with_fuse_limits(mut self, max_background: u16, congestion_threshold: u16) -> Self {
        self.fuse_max_background = Some(max_background);
        self.fuse_congestion_threshold = Some(congestion_threshold);
        self
    }

    pub async fn mount(self, _mountpoint: &str) -> MountResult<MountHandle> {
        Err(not_implemented())
    }

    pub async fn mount_spawned(self, _mountpoint: &str) -> MountResult<SpawnedMountHandle> {
        Err(not_implemented())
    }
}
