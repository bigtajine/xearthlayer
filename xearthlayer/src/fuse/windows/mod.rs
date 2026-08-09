//! Windows virtual filesystem, backed by Dokan.
//!
//! Mirrors the fuse3 passthrough filesystem's behavior:
//! - Real files under `source_dir` are passed through directly.
//! - `.dds` textures that don't exist on disk are parsed as Web Mercator tile
//!   coordinates and generated on demand via the async DDS pipeline.
//!
//! Dokan's callback trait ([`dokan::FileSystemHandler`]) is synchronous and is
//! invoked from driver-managed OS threads (not Tokio workers), so DDS
//! generation blocks on a captured [`tokio::runtime::Handle`] rather than
//! `.await`-ing directly.
//!
//! ponytail: [`OrthoUnionFS`] (the consolidated multi-package mount used in
//! production by `manager::mounts`) is still a stub returning "not
//! implemented" — this only wires up the simpler single-directory
//! [`PassthroughFS`]. Upgrade path: same `FileSystemHandler` pattern, source
//! path resolution from `OrthoUnionIndex` instead of a single `source_dir`.
//! ponytail: no request coalescing (unlike the Linux fuse3 impl) — add a
//! `RequestCoalescer` here if concurrent duplicate requests become a problem.

use std::fs;
use std::future::Future;
use std::io;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use dokan::{
    CreateFileInfo, DiskSpaceInfo, FileInfo, FileSystemHandler, FileSystemMounter, FillDataResult,
    FindData, MountFlags, MountOptions, OperationInfo, OperationResult, VolumeInfo,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use widestring::{U16CStr, U16CString};
use winapi::shared::ntstatus::{
    STATUS_ACCESS_DENIED, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_NOT_FOUND,
};
use winapi::um::winnt::{
    ACCESS_MASK, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
};

use crate::coord::TileCoord;
use crate::executor::DdsClient;
use crate::fuse::{get_default_placeholder, parse_dds_filename, validate_dds_or_placeholder, DdsFilename};
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

fn chunk_to_tile_coords(coords: &DdsFilename) -> TileCoord {
    TileCoord {
        row: coords.row / 16,
        col: coords.col / 16,
        zoom: coords.zoom.saturating_sub(4),
    }
}

/// Block on an async DDS generation request from a synchronous Dokan callback thread.
fn request_dds_blocking(
    runtime: &tokio::runtime::Handle,
    dds_client: &Arc<dyn DdsClient>,
    timeout: Duration,
    tile_request_callback: Option<&TileRequestCallback>,
    coords: &DdsFilename,
) -> Vec<u8> {
    let tile = chunk_to_tile_coords(coords);
    if let Some(callback) = tile_request_callback {
        callback(tile);
    }

    let cancellation = CancellationToken::new();
    let rx = dds_client.request_dds(tile, cancellation.clone());

    let data = runtime.block_on(async {
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => response.data,
            _ => {
                cancellation.cancel();
                get_default_placeholder()
            }
        }
    });

    validate_dds_or_placeholder(data, "dokan")
}

/// Context associated with an open file/directory handle.
pub enum FileContext {
    Directory,
    RealFile(PathBuf),
    VirtualDds(DdsFilename),
}

fn to_relative_path(file_name: &U16CStr) -> PathBuf {
    let s = file_name.to_string_lossy();
    let trimmed = s.trim_start_matches(['\\', '/']);
    PathBuf::from(trimmed)
}

fn system_time_or_now(result: io::Result<SystemTime>) -> SystemTime {
    result.unwrap_or_else(|_| SystemTime::now())
}

fn metadata_to_file_info(metadata: &fs::Metadata) -> FileInfo {
    let attributes = if metadata.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_READONLY
    };
    FileInfo {
        attributes,
        creation_time: system_time_or_now(metadata.created()),
        last_access_time: system_time_or_now(metadata.accessed()),
        last_write_time: system_time_or_now(metadata.modified()),
        file_size: metadata.len(),
        number_of_links: 1,
        file_index: 0,
    }
}

fn virtual_dds_file_info(size: u64) -> FileInfo {
    let now = SystemTime::now();
    FileInfo {
        attributes: FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_READONLY,
        creation_time: now,
        last_access_time: now,
        last_write_time: now,
        file_size: size,
        number_of_links: 1,
        file_index: 0,
    }
}

/// Dokan-backed passthrough filesystem overlaying a single scenery pack directory.
///
/// Windows counterpart of the Linux fuse3 `Fuse3PassthroughFS`: real files pass
/// through from `source_dir`, missing `.dds` files are generated on demand.
pub struct PassthroughFS {
    source_dir: PathBuf,
    dds_client: Arc<dyn DdsClient>,
    expected_dds_size: u64,
    timeout: Duration,
    tile_request_callback: Option<TileRequestCallback>,
    runtime: tokio::runtime::Handle,
}

impl PassthroughFS {
    pub fn new(source_dir: PathBuf, dds_client: Arc<dyn DdsClient>, expected_dds_size: usize) -> Self {
        Self {
            source_dir,
            dds_client,
            expected_dds_size: expected_dds_size as u64,
            timeout: Duration::from_secs(30),
            tile_request_callback: None,
            runtime: tokio::runtime::Handle::current(),
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

    pub async fn mount(self, mountpoint: &str) -> MountResult<MountHandle> {
        self.mount_spawned(mountpoint)
            .await
            .map(|spawned| MountHandle { spawned })
    }

    pub async fn mount_spawned(self, mountpoint: &str) -> MountResult<SpawnedMountHandle> {
        spawn_mount(mountpoint, self)
    }
}

impl<'c, 'h: 'c> FileSystemHandler<'c, 'h> for PassthroughFS {
    type Context = FileContext;

    fn create_file(
        &'h self,
        file_name: &U16CStr,
        _security_context: &dokan_sys::DOKAN_IO_SECURITY_CONTEXT,
        _desired_access: ACCESS_MASK,
        _file_attributes: u32,
        _share_access: u32,
        _create_disposition: u32,
        _create_options: u32,
        _info: &mut OperationInfo<'c, 'h, Self>,
    ) -> OperationResult<CreateFileInfo<Self::Context>> {
        let relative = to_relative_path(file_name);
        let full_path = self.source_dir.join(&relative);

        if let Ok(metadata) = fs::metadata(&full_path) {
            let is_dir = metadata.is_dir();
            return Ok(CreateFileInfo {
                context: if is_dir {
                    FileContext::Directory
                } else {
                    FileContext::RealFile(full_path)
                },
                is_dir,
                new_file_created: false,
            });
        }

        let name_str = relative
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if name_str.ends_with(".dds") {
            if let Ok(coords) = parse_dds_filename(&name_str) {
                return Ok(CreateFileInfo {
                    context: FileContext::VirtualDds(coords),
                    is_dir: false,
                    new_file_created: false,
                });
            }
        }

        Err(STATUS_OBJECT_NAME_NOT_FOUND)
    }

    fn get_file_information(
        &'h self,
        _file_name: &U16CStr,
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<FileInfo> {
        match context {
            FileContext::Directory => {
                let metadata = fs::metadata(&self.source_dir).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
                Ok(metadata_to_file_info(&metadata))
            }
            FileContext::RealFile(path) => {
                let metadata = fs::metadata(path).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
                Ok(metadata_to_file_info(&metadata))
            }
            FileContext::VirtualDds(_) => Ok(virtual_dds_file_info(self.expected_dds_size)),
        }
    }

    fn find_files(
        &'h self,
        file_name: &U16CStr,
        mut fill_find_data: impl FnMut(&FindData) -> FillDataResult,
        _info: &OperationInfo<'c, 'h, Self>,
        _context: &'c Self::Context,
    ) -> OperationResult<()> {
        let relative = to_relative_path(file_name);
        let dir_path = self.source_dir.join(&relative);

        let entries = fs::read_dir(&dir_path).map_err(|_| STATUS_NOT_A_DIRECTORY)?;

        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(name) = U16CString::from_os_str(entry.file_name()) else {
                continue;
            };
            let find_data = FindData {
                attributes: if metadata.is_dir() {
                    FILE_ATTRIBUTE_DIRECTORY
                } else {
                    FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_READONLY
                },
                creation_time: system_time_or_now(metadata.created()),
                last_access_time: system_time_or_now(metadata.accessed()),
                last_write_time: system_time_or_now(metadata.modified()),
                file_size: metadata.len(),
                file_name: name,
            };
            let _ = fill_find_data(&find_data);
        }

        Ok(())
    }

    fn read_file(
        &'h self,
        _file_name: &U16CStr,
        offset: i64,
        buffer: &mut [u8],
        _info: &OperationInfo<'c, 'h, Self>,
        context: &'c Self::Context,
    ) -> OperationResult<u32> {
        match context {
            FileContext::Directory => Err(STATUS_ACCESS_DENIED),
            FileContext::RealFile(path) => {
                let mut file = fs::File::open(path).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
                file.seek(SeekFrom::Start(offset as u64))
                    .map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
                let read = file.read(buffer).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
                Ok(read as u32)
            }
            FileContext::VirtualDds(coords) => {
                let data = request_dds_blocking(
                    &self.runtime,
                    &self.dds_client,
                    self.timeout,
                    self.tile_request_callback.as_ref(),
                    coords,
                );
                let offset = offset as usize;
                if offset >= data.len() {
                    return Ok(0);
                }
                let end = std::cmp::min(offset + buffer.len(), data.len());
                let n = end - offset;
                buffer[..n].copy_from_slice(&data[offset..end]);
                Ok(n as u32)
            }
        }
    }

    fn get_disk_free_space(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<DiskSpaceInfo> {
        match crate::system::filesystem::fs_info(&self.source_dir) {
            Ok(info) => Ok(DiskSpaceInfo {
                byte_count: info.total_bytes,
                free_byte_count: info.available_bytes,
                available_byte_count: info.available_bytes,
            }),
            Err(_) => Err(STATUS_ACCESS_DENIED),
        }
    }

    fn get_volume_information(&'h self, _info: &OperationInfo<'c, 'h, Self>) -> OperationResult<VolumeInfo> {
        Ok(VolumeInfo {
            name: U16CString::from_str("XEarthLayer").unwrap_or_default(),
            serial_number: 0x5845_4C31, // "XEL1"
            max_component_length: 255,
            fs_flags: 0,
            fs_name: U16CString::from_str("NTFS").unwrap_or_default(),
        })
    }
}

/// Spawn a Dokan mount on a dedicated OS thread and return a handle to control it.
///
/// Dokan's [`FileSystemMounter::mount`] borrows the handler for the lifetime of the
/// mount, so the handler is leaked to obtain a `'static` reference (one leak per
/// mount, for the process lifetime of that mount — acceptable for a long-running
/// daemon that mounts a small, fixed number of scenery packs).
fn spawn_mount(mountpoint: &str, handler: PassthroughFS) -> MountResult<SpawnedMountHandle> {
    let handler: &'static PassthroughFS = Box::leak(Box::new(handler));
    let mount_point = U16CString::from_str(mountpoint)
        .map_err(|e| MountError::InvalidPath(e.to_string()))?;
    let mount_point: &'static U16CString = Box::leak(Box::new(mount_point));

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<MountResult<()>>();
    let (unmount_tx, unmount_rx) = std::sync::mpsc::channel::<()>();
    let mountpoint_owned = mountpoint.to_string();

    let join_handle = std::thread::spawn(move || {
        dokan::init();

        let options = MountOptions {
            flags: MountFlags::WRITE_PROTECT,
            ..Default::default()
        };

        let mut mounter = FileSystemMounter::new(handler, mount_point, &options);
        let file_system = match mounter.mount() {
            Ok(fs) => fs,
            Err(e) => {
                let _ = ready_tx.send(Err(MountError::MountFailed(format!("{e:?}"))));
                dokan::shutdown();
                return;
            }
        };

        let _ = ready_tx.send(Ok(()));

        // Block until told to unmount; Dokan services requests on its own threads.
        let _ = unmount_rx.recv();

        let _ = dokan::unmount(mount_point);
        drop(file_system);
        dokan::shutdown();
    });

    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(MountError::MountFailed(
                "mount thread exited before signaling readiness".to_string(),
            ))
        }
    }

    Ok(SpawnedMountHandle {
        unmount_tx: Some(unmount_tx),
        join_handle: Some(join_handle),
        mountpoint: mountpoint_owned,
    })
}

pub struct MountHandle {
    spawned: SpawnedMountHandle,
}

impl Future for MountHandle {
    type Output = io::Result<()>;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // The Dokan mount runs on its own OS thread until unmounted; there is
        // nothing to poll here (unlike fuse3, Dokan has no async event loop
        // handle). Report ready immediately — callers that need to block
        // until unmount should hold `SpawnedMountHandle` instead.
        Poll::Ready(Ok(()))
    }
}

pub struct SpawnedMountHandle {
    unmount_tx: Option<std::sync::mpsc::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
    mountpoint: String,
}

impl SpawnedMountHandle {
    pub async fn unmount(mut self) -> io::Result<()> {
        if let Some(tx) = self.unmount_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            let mountpoint = self.mountpoint.clone();
            tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            })
            .await
            .map_err(|e| io::Error::other(format!("mount thread join failed: {e}")))?;
            tracing::debug!(mountpoint = %mountpoint, "Dokan mount thread joined");
        }
        Ok(())
    }
}

impl Drop for SpawnedMountHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.unmount_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

// =============================================================================
// OrthoUnionFS - not yet ported (see module docs)
// =============================================================================

fn not_implemented() -> MountError {
    MountError::MountFailed(
        "Dokan OrthoUnionFS (consolidated multi-package mount) is not implemented yet — \
         only the single-directory PassthroughFS is wired up so far."
            .to_string(),
    )
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
    dds_access_tx: Option<tokio::sync::mpsc::UnboundedSender<CoreDdsAccessEvent>>,
    #[allow(dead_code)]
    scene_tracker_tx: Option<tokio::sync::mpsc::UnboundedSender<FuseAccessEvent>>,
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

    pub fn with_dds_access_channel(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<CoreDdsAccessEvent>,
    ) -> Self {
        self.dds_access_tx = Some(tx);
        self
    }

    pub fn with_scene_tracker_channel(
        mut self,
        tx: tokio::sync::mpsc::UnboundedSender<FuseAccessEvent>,
    ) -> Self {
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
