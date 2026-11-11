//! Durable, link-safe writes, interrupts and plan fingerprints.

mod common;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{overlays, plan, signals};

#[test]
fn writes_replace_links_instead_of_following_them() {
    let f = rock4d();
    f.write("/etc/elsewhere", "keep\n");
    // armbianEnv.txt is a link, and a stale temporary file is a link too.
    std::fs::remove_file(f.p("/boot/armbianEnv.txt")).unwrap();
    f.write("/boot/real-env.txt", ROCK4D_ENV);
    f.link("/boot/armbianEnv.txt", "real-env.txt");
    f.link("/boot/.armbianEnv.txt.hwtool-new", "../etc/elsewhere");
    plan::write_file(&f.root(), "/boot/armbianEnv.txt", "new\n").unwrap();
    assert_eq!(f.read("/etc/elsewhere"), "keep\n");
    assert_eq!(f.read("/boot/real-env.txt"), ROCK4D_ENV);
    let meta = std::fs::symlink_metadata(f.p("/boot/armbianEnv.txt")).unwrap();
    assert!(meta.file_type().is_file());
    assert_eq!(f.read("/boot/armbianEnv.txt"), "new\n");
    assert!(!f.p("/boot/.armbianEnv.txt.hwtool-new").exists());
}

#[test]
fn fingerprints_ignore_time_but_not_content() {
    let f = rock4d();
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay/rockchip-rk3576-uart1.dtbo"),
        "",
    );
    let a = overlays::enable(&f.root(), &["uart1".into()]).unwrap();
    let b = overlays::enable(&f.root(), &["uart1".into()]).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}verbosity=7\n"),
    );
    let c = overlays::enable(&f.root(), &["uart1".into()]).unwrap();
    assert_ne!(a.fingerprint(), c.fingerprint());
    assert_eq!(
        plan::mask_stamps("x/20261111T041751Z/y"),
        plan::mask_stamps("x/20270101T000000Z-3/y")
    );
}

#[test]
fn a_signal_stops_before_the_next_step() {
    let f = rock4d();
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay/rockchip-rk3576-uart1.dtbo"),
        "",
    );
    let p = overlays::enable(&f.root(), &["uart1".into()]).unwrap();
    signals::raise_for_test();
    let err = p
        .execute(&f.root(), &mut RecordingExecutor::default(), "S")
        .unwrap_err();
    signals::reset();
    assert!(
        format!("{err:#}").contains("interrupted by a signal before step 1"),
        "{err:#}"
    );
    assert_eq!(f.read("/boot/armbianEnv.txt"), ROCK4D_ENV);
    // The log of the run exists.
    assert!(f
        .read("/var/log/hwtool/S.log")
        .contains("Failed: interrupted"));
}
