//! What the PC's graphics are, for the webview's sake.
//!
//! WebKitGTK's DMA-BUF renderer left the app's window blank on VirtualBox's
//! virtual GPU (`vmwgfx`, Ubuntu 26.04 under GNOME) after a few moves in the
//! patch bay, the page still running behind it; with the renderer off it
//! never did. So on a virtual machine's GPU the app starts with it off
//! (`WEBKIT_DISABLE_DMABUF_RENDERER`), and keeps it on everywhere else.

use std::path::Path;

/// The kernel drivers of virtual machines' GPUs: VMware's and VirtualBox's
/// VMSVGA (`vmwgfx`), VirtualBox's older adapters, QEMU's and KVM's
/// (`virtio_gpu`, `qxl`, `bochs`, `cirrus`), Hyper-V's; and the firmware's
/// framebuffer (`simpledrm`, whose driver is `simple-framebuffer`), all a VM
/// may have before its own, and on a real PC only with no GPU driver at all,
/// drawing in software, where the renderer gains nothing.
const VIRTUAL_DRIVERS: &[&str] = &[
    "vmwgfx",
    "vboxvideo",
    "virtio_gpu",
    "qxl",
    "bochs",
    "bochs-drm",
    "cirrus",
    "cirrus-qemu",
    "hyperv_drm",
    "simple-framebuffer",
];

/// Whether the GPU this session draws on is a virtual machine's: every
/// graphics card the kernel lists is driven by one of [`VIRTUAL_DRIVERS`]
/// (a PC passing a real GPU through to a VM has a real driver too).
pub fn is_virtual() -> bool {
    let drivers = card_drivers(Path::new("/sys/class/drm"));
    !drivers.is_empty()
        && drivers
            .iter()
            .all(|d| VIRTUAL_DRIVERS.contains(&d.as_str()))
}

/// The driver of each `cardN` under `drm` (not its connectors, `cardN-…`).
fn card_drivers(drm: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(drm) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|e| {
            let driver = std::fs::read_link(e.path().join("device/driver")).ok()?;
            Some(driver.file_name()?.to_string_lossy().into_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(drm: &Path, name: &str, driver: &str) {
        let device = drm.join(name).join("device");
        std::fs::create_dir_all(&device).unwrap();
        let target = drm.join("drivers").join(driver);
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, device.join("driver")).unwrap();
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("couch-gpu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cards_are_read_and_connectors_skipped() {
        let drm = temp("cards");
        card(&drm, "card0", "vmwgfx");
        card(&drm, "card0-Virtual-1", "ignored");
        card(&drm, "card1", "amdgpu");
        let mut drivers = card_drivers(&drm);
        drivers.sort();
        assert_eq!(drivers, ["amdgpu", "vmwgfx"]);
        let _ = std::fs::remove_dir_all(&drm);
    }

    #[test]
    fn no_cards_reads_as_none() {
        let drm = temp("none");
        assert!(card_drivers(&drm).is_empty());
        assert!(card_drivers(&drm.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&drm);
    }
}
