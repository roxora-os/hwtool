//! The UEFI image (roxora-os/build generic-rk3576-uefi), against a fake root.

mod common;

use common::*;
use hwtool_core::bootloader::{self, Target};
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{bootenv, bootinfo, fan, interfaces, kernel, migrate, overlays, uefi};

fn dtb(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/../../tests/interfaces/dtb/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// /proc/device-tree as the kernel shows the tree from `blob`, with the
/// /chosen node the firmware adds.
fn proc_dt(f: &Fake, blob: &[u8]) {
    for (k, v) in uefi::parse_fdt(blob).unwrap() {
        let p = f.p(&format!("/proc/device-tree{k}"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, v).unwrap();
    }
    f.write("/proc/device-tree/chosen/bootargs", "root=UUID=x ro\0");
}

/// The image's layout: edge kernel, GRUB on the ESP, no U-Boot, no boot.scr.
fn uefi_image(board: &str) -> Fake {
    let f = Fake {
        dir: tempfile::TempDir::new().unwrap(),
    };
    f.write(
        "/etc/armbian-release",
        &format!("BOARD={board}\nBOARD_NAME=\"RK3576 UEFI\"\nBOARDFAMILY=rk35xx\nBOOT_SOC=rk3576\nKERNEL_TARGET=edge\n"),
    );
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("grub-efi-arm64", "2.12-1ubuntu7"),
    ]);
    f.write(&format!("/boot/vmlinuz-{EDGE_KERNEL}"), "kernel");
    f.write("/boot/efi/EFI/ubuntu/grubaa64.efi", "grub");
    f.write("/boot/grub/grub.cfg", "menuentry 'Roxora' {}\n");
    f.programs(&["update-grub"]);
    std::fs::create_dir_all(f.p("/boot/efi/dtb/rockchip")).unwrap();
    std::fs::write(
        f.p("/boot/efi/dtb/rockchip/rk3576-rock-4d.dtb"),
        dtb("rk3576-rock-4d.dtb"),
    )
    .unwrap();
    std::fs::write(
        f.p("/boot/efi/dtb/rockchip/rk3576-armsom-cm5-io.dtb"),
        dtb("rk3576-armsom-cm5-io.dtb"),
    )
    .unwrap();
    f.write("/sys/firmware/efi/fw_platform_size", "64\n");
    f.write(
        "/sys/class/dmi/id/bios_vendor",
        "EFI Development Kit II / Radxa\n",
    );
    f.write("/sys/class/dmi/id/bios_version", "v0.3\n");
    f.mounts("/dev/mmcblk0p2 / ext4 rw 0 0\n/dev/mmcblk0p1 /boot/efi vfat rw 0 0\n");
    f
}

#[test]
fn detected_and_dt_override_recognised() {
    let f = uefi_image("generic-rk3576-uefi");
    proc_dt(&f, &dtb("rk3576-rock-4d.dtb"));
    assert!(uefi::is_uefi(&f.root()));
    let st = uefi::status(&f.root());
    assert!(st.uefi && st.booted_with_uefi);
    assert_eq!(st.device_tree, "override");
    assert_eq!(
        st.override_file.as_deref(),
        Some("/boot/efi/dtb/rockchip/rk3576-rock-4d.dtb")
    );
    assert_eq!(st.soc.as_deref(), Some("rk3576"));
    assert_eq!(st.firmware_version.as_deref(), Some("v0.3"));
    assert_eq!(st.esp_dtbs.len(), 2);
}

#[test]
fn firmware_tree_when_no_esp_dtb_matches() {
    let f = uefi_image("generic-rk3576-uefi");
    proc_dt(&f, &dtb("rk3576-rock-4d.dtb"));
    std::fs::remove_file(f.p("/boot/efi/dtb/rockchip/rk3576-rock-4d.dtb")).unwrap();
    let st = uefi::status(&f.root());
    assert_eq!(st.device_tree, "firmware");
    assert!(st.override_file.is_none());
    assert!(st.similarity.unwrap() < 0.98, "{:?}", st.similarity);
}

#[test]
fn detected_without_the_board_name() {
    let f = uefi_image("some-rk3576-board");
    assert!(uefi::is_uefi(&f.root()));
    // A U-Boot image is not UEFI.
    assert!(!uefi::is_uefi(&rock4d().root()));
    assert!(!uefi::is_uefi(&cm5().root()));
}

#[test]
fn u_boot_and_boot_script_commands_refuse() {
    let f = uefi_image("generic-rk3576-uefi");
    let r = f.root();
    let mut ex = RecordingExecutor::default();
    let overlay_errs = [
        overlays::status(&r).err(),
        overlays::enable(&r, &["uart1".into()]).err(),
        overlays::remove_plan(&r, "x").err(),
        interfaces::status(&r).err(),
        interfaces::enable_plan(&r, &["i2c8-m2".into()]).err(),
    ];
    for e in overlay_errs {
        let e = e.expect("refused").to_string();
        assert!(e.contains("overlays are applied by the firmware"), "{e}");
    }
    let e = fan::enable_plan(&r, &mut ex, None, fan::DEFAULT_CURVE)
        .unwrap_err()
        .to_string();
    assert!(e.contains("overlays are applied by the firmware"), "{e}");
    let e = bootenv::status(&r).unwrap_err().to_string();
    assert!(e.contains("no boot.scr or armbianEnv.txt"), "{e}");
    for e in [
        bootloader::write_plan(&r, &Target::Spi, false).err(),
        bootinfo::versions(&r).err(),
        bootinfo::update_plan(&r).err(),
        bootinfo::order(&r, &mut ex).err(),
    ] {
        let e = e.expect("refused").to_string();
        assert!(e.contains("there is no U-Boot"), "{e}");
    }
    let e = kernel::switch_plan(&r, &mut ex, "vendor", false, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("vendor branch has no UEFI support"), "{e}");
    assert!(kernel::revert_plan(&r, &mut ex, false).is_err());
    let e = migrate::plan(&r, &mut ex, "/dev/nvme0n1", false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("UEFI image"), "{e}");
}

#[test]
fn kernel_status_and_hold_keep_working() {
    let f = uefi_image("generic-rk3576-uefi");
    let st = kernel::status(&f.root()).unwrap();
    assert_eq!(st.board.board, "generic-rk3576-uefi");
    assert_eq!(st.branches.len(), 1);
    assert_eq!(st.branches[0].branch, "edge");
    let text = kernel::hold_plan(&f.root(), true).unwrap().render();
    assert!(text.contains("linux-image-edge-rockchip64"), "{text}");
    let st = bootloader::status(&f.root());
    assert!(st.uboot.is_none());
}
