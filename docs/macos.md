# XEarthLayer on macOS

XEarthLayer supports macOS (Apple Silicon tested) using [macFUSE](https://macfuse.io) to provide the FUSE virtual filesystem that streams textures to X-Plane.

## Requirements

- macOS 12 or later (tested on Apple Silicon; Intel Macs should work but are untested)
- [macFUSE](https://macfuse.io) 5.x or later
- X-Plane 12

## Installing macFUSE

macFUSE ships as a kernel extension (kext), so installation involves a one-time
security approval that is stricter on Apple Silicon Macs.

### 1. Install the package

Download the installer from [macfuse.io](https://macfuse.io) and run it, or use Homebrew:

```bash
brew install --cask macfuse
```

### 2. Approve the kernel extension

**Apple Silicon Macs** must first allow third-party kernel extensions:

1. Shut down the Mac completely.
2. Press and **hold** the power button until "Loading startup options" appears, then choose **Options** to boot into Recovery.
3. Open **Utilities → Startup Security Utility**, select your startup disk, and choose **Reduced Security** with **"Allow user management of kernel extensions from identified developers"** checked.
4. Restart back into macOS.

**All Macs** then approve the extension:

1. Open **System Settings → Privacy & Security**.
2. Under Security you will see a message that system software from developer **"Benjamin Fleischer"** (the macFUSE developer) was blocked — click **Allow** (macOS may ask you to **Enable System Extensions** and restart).
3. Restart when prompted.

After the restart, verify macFUSE is active — the mount helper should exist:

```bash
ls /usr/local/lib/libfuse* /Library/Filesystems/macfuse.fs 2>/dev/null
```

If the approval prompt never appeared, re-run the macFUSE installer after completing the Recovery steps.

### 3. Build and run XEarthLayer

Building from source works exactly as on Linux:

```bash
make release
make install   # installs to ~/.local/bin
xearthlayer setup
```

The setup wizard auto-detects your X-Plane installation, total memory (via `sysctl`), and storage type (via `diskutil`) to suggest sensible cache settings.

## Platform Behavior Notes

### Texture caching uses the kernel page cache

On Linux, XEarthLayer serves generated DDS textures with direct I/O so every
read from X-Plane is visible to the streaming service. On macOS, direct I/O
interacts badly with X-Plane's memory-mapped texture loading (it can crash the
simulator), so generated textures are served through the normal kernel page
cache instead.

This is safe and transparent, with one side effect: once a texture is cached by
the kernel, repeat reads never reach XEarthLayer. Position inference from file
access patterns (`InferenceAdapter`) therefore only sees the *first* read of
each tile, so in steady flight XEarthLayer relies on the X-Plane **Web API**
for aircraft position. The Web API connection is automatic and requires no
configuration — just make sure X-Plane's Web API is not disabled. Prefetching
and adaptive scenery loading work normally through it.

### Stale mount recovery

If XEarthLayer is killed without unmounting (crash, force-quit), macFUSE leaves
a stale mount behind and the next run would fail with *"Device not configured
(os error 6)"*. XEarthLayer detects this at startup and force-unmounts the
stale mountpoint automatically. If you ever need to do it manually:

```bash
umount "/path/to/X-Plane 12/Custom Scenery/zzXEL_ortho"
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `mount_macfuse: the file system is not available (255)` | macFUSE kext not approved — redo the approval steps above |
| "System extension blocked" notification | System Settings → Privacy & Security → Allow, then restart |
| No approval option appears (Apple Silicon) | Boot into Recovery and enable Reduced Security with kernel extension management first |
| "Device not configured (os error 6)" on mount | Stale mount from a previous run — recovered automatically at startup; manual fix: `umount <mountpoint>` |
| macFUSE stops working after a macOS upgrade | Re-run the macFUSE installer and re-approve the extension |
