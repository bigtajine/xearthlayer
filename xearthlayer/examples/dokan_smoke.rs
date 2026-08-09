//! Real end-to-end smoke test for the Dokan-backed Windows filesystem.
//!
//! Not a `#[test]` because it mounts a real drive letter via the Dokan
//! kernel driver — needs the driver installed and is easiest to run/observe
//! as a standalone binary. Run with:
//!
//! ```text
//! cargo run --example dokan_smoke
//! ```
//!
//! Exits non-zero (via `assert!`) on any mismatch.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("dokan_smoke is Windows-only");
}

#[cfg(target_os = "windows")]
#[tokio::main]
async fn main() {
    use std::fs;
    use std::sync::Arc;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    use xearthlayer::coord::TileCoord;
    use xearthlayer::executor::{DdsClient, DdsClientError, Priority};
    use xearthlayer::fuse::PassthroughFS;
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
            // Deliberately not a valid DDS — exercises validate_dds_or_placeholder's
            // fallback path, so the mount round-trips a real (placeholder) texture.
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

    let source_dir = std::env::temp_dir().join(format!("xel_dokan_smoke_{}", std::process::id()));
    fs::create_dir_all(source_dir.join("sub")).expect("create source dir");
    fs::write(source_dir.join("hello.txt"), b"hello from xearthlayer").expect("write real file");
    fs::write(source_dir.join("sub").join("nested.txt"), b"nested content").expect("write nested file");

    let (tile_tx, mut tile_rx) = mpsc::unbounded_channel();
    let dds_client: Arc<dyn DdsClient> = Arc::new(FakeDdsClient { tx: tile_tx });

    let fs_handler = PassthroughFS::new(source_dir.clone(), dds_client, 11_174_016);

    println!("Mounting {} at K:\\ ...", source_dir.display());
    let mount = fs_handler
        .mount_spawned("K:\\")
        .await
        .expect("mount should succeed");

    // Give the driver a moment to attach the volume.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let real = fs::read_to_string("K:\\hello.txt").expect("read real passthrough file");
    assert_eq!(real, "hello from xearthlayer");
    println!("PASS: real file passthrough read: {real:?}");

    let nested = fs::read_to_string("K:\\sub\\nested.txt").expect("read nested real file");
    assert_eq!(nested, "nested content");
    println!("PASS: nested directory passthrough read: {nested:?}");

    let entries: Vec<String> = fs::read_dir("K:\\")
        .expect("readdir root")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    assert!(entries.contains(&"hello.txt".to_string()));
    assert!(entries.contains(&"sub".to_string()));
    println!("PASS: directory listing: {entries:?}");

    // A DDS filename in AutoOrtho/Ortho4XP format that doesn't exist on disk —
    // should be generated on demand (parsed, routed through FakeDdsClient,
    // and the invalid mock payload replaced by the real placeholder DDS).
    let dds_data = fs::read("K:\\100000_125184_BI18.dds").expect("read virtual dds file");
    assert_eq!(&dds_data[0..4], b"DDS ", "generated file should be a valid DDS");
    assert_eq!(dds_data.len(), 11_174_016);
    println!("PASS: virtual DDS generation: {} bytes, valid DDS magic", dds_data.len());

    let tile = tile_rx.try_recv().expect("DdsClient should have been invoked");
    assert_eq!(tile.row, 100000 / 16);
    assert_eq!(tile.col, 125184 / 16);
    println!("PASS: DdsClient received correct tile coords: {tile:?}");

    println!("Unmounting...");
    mount.unmount().await.expect("clean unmount");

    fs::remove_dir_all(&source_dir).ok();

    println!("\nALL CHECKS PASSED — Dokan PassthroughFS is working end-to-end.");
}
