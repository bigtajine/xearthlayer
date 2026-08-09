//! Real end-to-end smoke test for the Dokan-backed OrthoUnionFS — the
//! consolidated multi-package mount `manager::mounts` actually uses in
//! production. See `dokan_smoke.rs` for the simpler PassthroughFS test.
//!
//! ```text
//! cargo run --example ortho_union_smoke
//! ```

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ortho_union_smoke is Windows-only");
}

#[cfg(target_os = "windows")]
#[tokio::main]
async fn main() {
    use semver::Version;
    use std::fs;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    use xearthlayer::coord::TileCoord;
    use xearthlayer::executor::{DdsClient, DdsClientError, Priority};
    use xearthlayer::fuse::OrthoUnionFS;
    use xearthlayer::ortho_union::OrthoUnionIndexBuilder;
    use xearthlayer::package::{InstalledPackage, Package, PackageType};
    use xearthlayer::runtime::{DdsResponse, JobRequest, RequestOrigin};

    struct FakeDdsClient {
        tx: mpsc::UnboundedSender<TileCoord>,
    }

    impl DdsClient for FakeDdsClient {
        fn submit(&self, _request: JobRequest) -> Result<(), DdsClientError> {
            Ok(())
        }

        fn request_dds(
            &self,
            tile: TileCoord,
            _cancellation: CancellationToken,
        ) -> oneshot::Receiver<DdsResponse> {
            let _ = self.tx.send(tile);
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(DdsResponse::new(b"not a real dds".to_vec(), false, Default::default(), true));
            rx
        }

        fn request_dds_with_options(
            &self,
            tile: TileCoord,
            _priority: Priority,
            _origin: RequestOrigin,
            cancellation: CancellationToken,
        ) -> oneshot::Receiver<DdsResponse> {
            self.request_dds(tile, cancellation)
        }

        fn is_connected(&self) -> bool {
            true
        }
    }

    let root = std::env::temp_dir().join(format!("xel_ortho_smoke_{}", std::process::id()));
    let pkg_dir = root.join("na_ortho");
    fs::create_dir_all(pkg_dir.join("Earth nav data/+40-080")).expect("create package dir");
    fs::write(
        pkg_dir.join("Earth nav data/+40-080/+40-074.dsf"),
        b"fake dsf payload",
    )
    .expect("write dsf");
    fs::create_dir_all(pkg_dir.join("terrain")).expect("create terrain dir");
    fs::write(pkg_dir.join("terrain/package.ter"), b"fake terrain").expect("write ter");

    let package = InstalledPackage::new(
        Package::new("na", PackageType::Ortho, Version::new(1, 0, 0)),
        &pkg_dir,
    );

    let index = OrthoUnionIndexBuilder::new()
        .add_package(package)
        .build()
        .expect("build ortho union index");
    println!(
        "Index built: {} sources, {} files",
        index.source_count(),
        index.file_count()
    );

    let (tile_tx, mut tile_rx) = mpsc::unbounded_channel();
    let dds_client: Arc<dyn DdsClient> = Arc::new(FakeDdsClient { tx: tile_tx });

    let fs_handler = OrthoUnionFS::new(index, dds_client, 11_174_016);

    // Mount onto an existing empty NTFS folder rather than a drive letter —
    // this is what manager::mounts does for real, joining the mount under
    // X-Plane's Custom Scenery directory as `.../zzXEL_ortho`.
    let mount_root = root.join("Custom Scenery");
    fs::create_dir_all(mount_root.join("zzXEL_ortho")).expect("create mount folder");
    let mountpoint = mount_root.join("zzXEL_ortho");
    let mountpoint_str = mountpoint.to_str().expect("mountpoint is valid utf-8");

    println!("Mounting consolidated ortho union at folder {mountpoint_str} ...");
    let mount = fs_handler
        .mount_spawned(mountpoint_str)
        .await
        .expect("mount should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let dsf = fs::read(mountpoint.join("Earth nav data").join("+40-080").join("+40-074.dsf"))
        .expect("read passthrough DSF");
    assert_eq!(dsf, b"fake dsf payload");
    println!("PASS: passthrough DSF read through union index: {} bytes", dsf.len());

    let entries: Vec<String> = fs::read_dir(&mountpoint)
        .expect("readdir root")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(entries.contains(&"Earth nav data".to_string()));
    assert!(entries.contains(&"terrain".to_string()));
    println!("PASS: root directory listing from union index: {entries:?}");

    let dds_data = fs::read(mountpoint.join("terrain").join("100000_125184_BI18.dds"))
        .expect("read virtual dds file");
    assert_eq!(&dds_data[0..4], b"DDS ");
    assert_eq!(dds_data.len(), 11_174_016);
    println!("PASS: virtual DDS generation via union index: {} bytes", dds_data.len());

    let tile = tile_rx.try_recv().expect("DdsClient should have been invoked");
    assert_eq!(tile.row, 100000 / 16);
    assert_eq!(tile.col, 125184 / 16);
    println!("PASS: DdsClient received correct tile coords: {tile:?}");

    println!("Unmounting...");
    mount.unmount().await.expect("clean unmount");

    fs::remove_dir_all(&root).ok();

    println!("\nALL CHECKS PASSED — Dokan OrthoUnionFS is working end-to-end.");
}
