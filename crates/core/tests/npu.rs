//! NPU status and charsiu plans against a fake root.

mod common;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{kernel, npu};

const POLICY: &str = "charsiu:\n  Installed: (none)\n  Candidate: 0.1.0\n  Version table:\n     0.1.0 500\n        500 http://apt.armbian.com noble/noble-utils arm64 Packages\n";
const POLICY_INSTALLED: &str = "charsiu:\n  Installed: 0.1.0\n  Candidate: 0.1.0\n  Version table:\n *** 0.1.0 500\n        500 http://apt.armbian.com noble/noble-utils arm64 Packages\n        100 /var/lib/dpkg/status\n";

fn exec(policy: &str) -> RecordingExecutor {
    let mut ex = RecordingExecutor::default();
    ex.answer("apt-cache policy charsiu", policy);
    ex
}

/// A ROCK 4D running edge with rocket, its two NPU cores and /dev/accel/accel0.
fn npu_board(status: &str) -> Fake {
    let f = rock4d();
    f.write("/proc/sys/kernel/osrelease", &format!("{EDGE_KERNEL}\n"));
    f.write(
        &format!("/lib/modules/{EDGE_KERNEL}/kernel/drivers/accel/rocket/rocket.ko"),
        "ko",
    );
    f.write(
        "/proc/modules",
        "rocket 49152 0 - Live 0x0000000000000000\n",
    );
    for n in ["npu@27700000", "npu@27708000"] {
        let d = format!("/proc/device-tree/soc/{n}");
        f.write(&format!("{d}/compatible"), "rockchip,rk3576-rknn-core\0");
        f.write(&format!("{d}/status"), &format!("{status}\0"));
    }
    f.write("/dev/accel/accel0", "");
    f
}

#[test]
fn ready_on_edge_with_rocket() {
    let f = npu_board("okay");
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(st.branch.as_deref(), Some("edge"));
    assert!(st.rocket.loaded && st.rocket.available());
    assert_eq!(st.nodes.len(), 2);
    assert_eq!(st.nodes[0].path, "/soc/npu@27700000");
    assert!(st.nodes.iter().all(|n| n.enabled()));
    assert_eq!(st.accel.len(), 1);
    assert!(st.ready && st.can_install);
    assert_eq!(
        st.verdict,
        "NPU ready for charsiu (sudo hwtool npu install-charsiu)"
    );
    assert_eq!(st.charsiu.candidate.as_deref(), Some("0.1.0"));
    assert_eq!(
        st.charsiu.origin.as_deref(),
        Some("http://apt.armbian.com noble/noble-utils arm64 Packages")
    );
}

#[test]
fn disabled_node() {
    let f = npu_board("disabled");
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert!(!st.can_install);
    assert_eq!(st.verdict, "the NPU is disabled in the device tree");
    let err = npu::install_plan(&f.root(), &mut exec(POLICY))
        .unwrap_err()
        .to_string();
    assert!(err.contains("disabled in the device tree"), "{err}");
}

#[test]
fn vendor_kernel_with_rknpu() {
    let f = rock4d();
    f.write("/proc/sys/kernel/osrelease", &format!("{VENDOR_KERNEL}\n"));
    f.write(
        &format!("/lib/modules/{VENDOR_KERNEL}/modules.builtin"),
        "drivers/rknpu/rknpu.ko\n",
    );
    f.write("/sys/module/rknpu/version", "0.9.8\n");
    f.write(
        "/proc/device-tree/npu@27700000/compatible",
        "rockchip,rk3576-rknpu\0",
    );
    f.write("/proc/device-tree/npu@27700000/status", "okay\0");
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(st.branch.as_deref(), Some("vendor"));
    assert!(st.rknpu.builtin && st.rknpu.loaded);
    assert!(!st.rocket.available());
    assert!(st
        .verdict
        .starts_with("vendor kernel: charsiu needs the edge kernel's rocket driver"));
    assert!(npu::install_plan(&f.root(), &mut exec(POLICY)).is_err());
}

#[test]
fn no_driver_and_no_node() {
    let f = rock4d();
    f.write("/proc/sys/kernel/osrelease", &format!("{EDGE_KERNEL}\n"));
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(
        st.verdict,
        "the kernel has no NPU driver (no rocket module)"
    );
    f.write(
        &format!("/lib/modules/{EDGE_KERNEL}/kernel/drivers/accel/rocket/rocket.ko.zst"),
        "ko",
    );
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(
        st.verdict,
        "the device tree has no NPU node for the rocket driver"
    );
}

#[test]
fn rk3588_top_level_node_and_not_loaded() {
    let f = rock4d();
    f.write("/proc/sys/kernel/osrelease", &format!("{EDGE_KERNEL}\n"));
    f.write(
        &format!("/lib/modules/{EDGE_KERNEL}/kernel/drivers/accel/rocket/rocket.ko"),
        "ko",
    );
    // No status property means okay.
    f.write(
        "/proc/device-tree/npu@fdab0000/compatible",
        "rockchip,rk3588-rknn-core\0",
    );
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(st.nodes.len(), 1);
    assert!(st.can_install && !st.ready);
    assert_eq!(
        st.verdict,
        "the rocket driver is not loaded (sudo modprobe rocket)"
    );
}

#[test]
fn chroot_or_container_looks_at_the_next_kernel() {
    let f = npu_board("okay");
    f.write("/proc/sys/kernel/osrelease", "6.11.0-1018-azure\n");
    let st = npu::status(&f.root(), &mut exec(POLICY)).unwrap();
    assert_eq!(st.kernel.as_deref(), Some(EDGE_KERNEL));
    assert_eq!(st.running_kernel.as_deref(), Some("6.11.0-1018-azure"));
    assert!(!st.rocket.loaded && st.can_install);
    assert!(st.verdict.contains("reboot into it"), "{}", st.verdict);
}

#[test]
fn install_plan_shows_apt_and_refuses_extra_packages() {
    let f = npu_board("okay");
    let mut ex = exec(POLICY);
    ex.answer(
        "apt-get -s -y --no-install-recommends install charsiu",
        "Inst charsiu (0.1.0 Armbian:noble [arm64])\nConf charsiu (0.1.0 Armbian:noble [arm64])\n",
    );
    let p = npu::install_plan(&f.root(), &mut ex).unwrap();
    let text = p.render();
    assert!(
        text.contains("$ apt-get install -y --no-install-recommends charsiu"),
        "{text}"
    );
    assert!(
        text.contains("  Inst charsiu (0.1.0 Armbian:noble [arm64])"),
        "{text}"
    );
    assert!(text.contains("charsiu pull"), "{text}");
    // No model is downloaded.
    assert!(!p.steps.iter().any(|s| matches!(&s.action, hwtool_core::plan::Action::Run { argv } if argv.iter().any(|a| a == "pull"))));

    ex.answer(
        "apt-get -s -y --no-install-recommends install charsiu",
        "Inst whiptail (0.52 Ubuntu:24.04 [arm64])\nInst charsiu (0.1.0 Armbian:noble [arm64])\n",
    );
    let err = npu::install_plan(&f.root(), &mut ex)
        .unwrap_err()
        .to_string();
    assert!(err.contains("would also do: Inst whiptail"), "{err}");
    ex.answer(
        "apt-get -s -y --no-install-recommends install charsiu",
        "Remv foo [1]\nInst charsiu (0.1.0 Armbian:noble [arm64])\n",
    );
    assert!(npu::install_plan(&f.root(), &mut ex)
        .unwrap_err()
        .to_string()
        .contains("Remv foo"));
    // Executing runs apt and then shows the status.
    ex.answer(
        "apt-get -s -y --no-install-recommends install charsiu",
        "Inst charsiu (0.1.0 Armbian:noble [arm64])\n",
    );
    let p = npu::install_plan(&f.root(), &mut ex).unwrap();
    ex.ran.clear();
    p.execute(&f.root(), &mut ex, "N").unwrap();
    assert_eq!(
        ex.ran[0].join(" "),
        "apt-get install -y --no-install-recommends charsiu"
    );
}

#[test]
fn remove_plan_and_already_installed() {
    let f = npu_board("okay");
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("charsiu", "0.1.0"),
    ]);
    let mut ex = exec(POLICY_INSTALLED);
    let err = npu::install_plan(&f.root(), &mut ex)
        .unwrap_err()
        .to_string();
    assert!(err.contains("already installed"), "{err}");
    ex.answer("apt-get -s -y remove charsiu", "Remv charsiu [0.1.0]\n");
    let text = npu::remove_plan(&f.root(), &mut ex).unwrap().render();
    assert!(text.contains("$ apt-get remove -y charsiu"), "{text}");
    assert!(text.contains("/opt/charsiu/models"), "{text}");
    ex.answer(
        "apt-get -s -y remove charsiu",
        "Remv charsiu [0.1.0]\nRemv libfoo [1]\n",
    );
    assert!(npu::remove_plan(&f.root(), &mut ex).is_err());
}

#[test]
fn switching_to_vendor_warns_about_charsiu() {
    let f = rock4d();
    f.status(&[
        ("linux-image-edge-rockchip64", "26.11.0-trunk"),
        ("linux-dtb-edge-rockchip64", "26.11.0-trunk"),
        ("linux-u-boot-radxa-rock-4d-edge", "26.11.0-trunk"),
        ("charsiu", "0.1.0"),
    ]);
    let mut ex = RecordingExecutor::default();
    for p in [
        "linux-image-vendor-rk35xx",
        "linux-dtb-vendor-rk35xx",
        "linux-u-boot-radxa-rock-4d-vendor",
    ] {
        ex.answer(
            &format!("apt-cache policy {p}"),
            &policy(Some("26.5.1"), &["26.5.1"]),
        );
    }
    ex.answer(
        "apt-get -s -y install linux-image-vendor-rk35xx linux-dtb-vendor-rk35xx linux-u-boot-radxa-rock-4d-vendor",
        "Remv linux-u-boot-radxa-rock-4d-edge [1]\nInst linux-image-vendor-rk35xx (26.5.1)\n",
    );
    ex.answer(
        "apt-get -s -y remove linux-image-edge-rockchip64 linux-dtb-edge-rockchip64",
        "Remv linux-image-edge-rockchip64 [1]\nRemv linux-dtb-edge-rockchip64 [1]\n",
    );
    let text = kernel::switch_plan(&f.root(), &mut ex, "vendor", false, None)
        .unwrap()
        .render();
    assert!(
        text.contains("charsiu 0.1.0 is installed. It needs the edge kernel's rocket driver"),
        "{text}"
    );
}
