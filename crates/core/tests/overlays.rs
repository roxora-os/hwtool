mod common;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::overlays;
use hwtool_core::plan::Action;

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn edge_dtb_has_no_rk3576_overlays() {
    let f = rock4d();
    let st = overlays::status(&f.root()).unwrap();
    assert_eq!(st.overlay_prefix, vec!["rockchip-rk3576"]);
    assert!(st.overlays.is_empty(), "{:?}", st.overlays);
    let err = overlays::enable(&f.root(), &names(&["uart1"]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no overlays for this board"), "{err}");
}

#[test]
fn prefixes_are_tried_in_order() {
    let f = cm5();
    let dir = format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay");
    f.write(&format!("{dir}/armsom-cm5-io-uart1.dtbo"), "");
    f.write(&format!("{dir}/rockchip-rk3576-uart1.dtbo"), "");
    f.write(&format!("{dir}/rockchip-rk3576-i2c2.dtbo"), "");
    let st = overlays::status(&f.root()).unwrap();
    let got: Vec<(String, String)> = st
        .overlays
        .iter()
        .map(|o| (o.name.clone(), o.file.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "i2c2".to_string(),
                "/boot/dtb/rockchip/overlay/rockchip-rk3576-i2c2.dtbo".to_string()
            ),
            (
                "uart1".to_string(),
                "/boot/dtb/rockchip/overlay/armsom-cm5-io-uart1.dtbo".to_string()
            ),
        ]
    );
}

#[test]
fn enable_backs_up_and_writes_env() {
    let f = rock4d();
    let dir = format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay");
    f.write(&format!("{dir}/rockchip-rk3576-uart1.dtbo"), "");
    f.write(&format!("{dir}/rockchip-rk3576-i2c2.dtbo"), "");
    let plan = overlays::enable(&f.root(), &names(&["uart1", "i2c2"])).unwrap();
    assert!(plan.reboot_required);
    assert_eq!(plan.steps.len(), 2);
    assert!(matches!(plan.steps[0].action, Action::Backup { .. }));
    let text = plan.render();
    assert!(text.contains("write /boot/armbianEnv.txt:"), "{text}");
    assert!(text.contains("| overlays=uart1 i2c2"), "{text}");

    let mut ex = RecordingExecutor::default();
    plan.execute(&f.root(), &mut ex, "T1").unwrap();
    assert!(ex.ran.is_empty(), "no commands for an overlay change");
    assert_eq!(
        f.read("/boot/armbianEnv.txt"),
        format!("{ROCK4D_ENV}overlays=uart1 i2c2\n")
    );
    assert_eq!(
        f.read("/var/backups/roxora-hwtool/T1/boot/armbianEnv.txt"),
        ROCK4D_ENV
    );

    let st = overlays::status(&f.root()).unwrap();
    assert!(st.overlays.iter().all(|o| o.enabled));

    let plan = overlays::disable(&f.root(), &names(&["uart1"])).unwrap();
    // Same time stamp as the first change: the first backup must survive.
    plan.execute(&f.root(), &mut ex, "T1").unwrap();
    assert_eq!(
        f.read("/var/backups/roxora-hwtool/T1/boot/armbianEnv.txt"),
        ROCK4D_ENV
    );
    assert_eq!(
        f.read("/var/backups/roxora-hwtool/T1-1/boot/armbianEnv.txt"),
        format!("{ROCK4D_ENV}overlays=uart1 i2c2\n")
    );
    assert_eq!(
        f.read("/boot/armbianEnv.txt"),
        format!("{ROCK4D_ENV}overlays=i2c2\n")
    );
    let plan = overlays::disable(&f.root(), &names(&["i2c2"])).unwrap();
    plan.execute(&f.root(), &mut ex, "T3").unwrap();
    assert_eq!(f.read("/boot/armbianEnv.txt"), ROCK4D_ENV);
}

#[test]
fn enabling_twice_changes_nothing() {
    let f = rock4d();
    f.write(
        &format!("/boot/dtb-{EDGE_KERNEL}/rockchip/overlay/rockchip-rk3576-uart1.dtbo"),
        "",
    );
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}overlays=uart1\n"),
    );
    let plan = overlays::enable(&f.root(), &names(&["uart1"])).unwrap();
    assert!(plan.is_empty());
    assert!(!plan.reboot_required);
}

#[test]
fn user_overlays() {
    let f = rock4d();
    f.write("/boot/overlay-user/my-sensor.dtbo", "");
    let plan = overlays::enable(&f.root(), &names(&["my-sensor"])).unwrap();
    plan.execute(&f.root(), &mut RecordingExecutor::default(), "T")
        .unwrap();
    assert!(f
        .read("/boot/armbianEnv.txt")
        .ends_with("user_overlays=my-sensor\n"));
    let st = overlays::status(&f.root()).unwrap();
    assert_eq!(st.overlays.len(), 1);
    assert_eq!(st.overlays[0].kind, "user");
    assert!(st.overlays[0].enabled);
}

#[test]
fn missing_and_invalid_names() {
    let f = rock4d();
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}overlays=gone\n"),
    );
    let st = overlays::status(&f.root()).unwrap();
    assert_eq!(st.missing, vec!["gone"]);
    assert!(overlays::disable(&f.root(), &names(&["other"])).is_err());
    for bad in ["a b", "../x", "x=y", ""] {
        assert!(
            overlays::enable(&f.root(), &names(&[bad])).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn no_env_file() {
    let f = rock4d();
    std::fs::remove_file(f.p("/boot/armbianEnv.txt")).unwrap();
    let err = overlays::status(&f.root()).unwrap_err().to_string();
    assert!(err.contains("does not exist"), "{err}");
}
