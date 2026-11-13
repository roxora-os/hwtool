//! PWM fan control, against a fake root; the generated overlays are
//! compiled with dtc -@ and applied with fdtoverlay to the boards' DTBs.

mod common;

use std::process::Command;

use common::*;
use hwtool_core::exec::RecordingExecutor;
use hwtool_core::{fan, interfaces, thermal};

fn with_interfaces(f: &Fake, board: &str) {
    let src = format!(
        "{}/../../data/interfaces/{board}",
        env!("CARGO_MANIFEST_DIR")
    );
    let dst = format!("/usr/share/hwtool/interfaces/{board}");
    for e in std::fs::read_dir(&src).unwrap() {
        let n = e.unwrap().file_name().into_string().unwrap();
        if n == "index.json" {
            f.write(
                &format!("{dst}/{n}"),
                &std::fs::read_to_string(format!("{src}/{n}")).unwrap(),
            );
        } else if let Some(stem) = n.strip_suffix(".dtso") {
            f.write(&format!("{dst}/{stem}.dtbo"), "dtbo");
        }
    }
}

/// Run the plan and return the overlay source it wrote.
fn enable(f: &Fake, pwm: Option<&str>, curve: &str) -> (String, String) {
    f.programs(&["dtc"]);
    let p = fan::enable_plan(&f.root(), &mut RecordingExecutor::default(), pwm, curve).unwrap();
    let text = p.render();
    p.execute(&f.root(), &mut RecordingExecutor::default(), "F")
        .unwrap();
    (text, f.read(fan::SOURCE))
}

/// Compile with dtc -@ and apply to the board's DTB; the merged tree as dts.
fn apply(src: &str, dtb: &str) -> Option<String> {
    if Command::new("dtc").arg("--version").output().is_err() {
        assert!(std::env::var_os("CI").is_none(), "CI must have dtc");
        return None;
    }
    let d = tempfile::TempDir::new().unwrap();
    let dtso = d.path().join("fan.dtso");
    std::fs::write(&dtso, src).unwrap();
    let dtbo = d.path().join("fan.dtbo");
    let ok = Command::new("dtc")
        .args(["-@", "-q", "-I", "dts", "-O", "dtb", "-o"])
        .arg(&dtbo)
        .arg(&dtso)
        .status()
        .unwrap();
    assert!(ok.success(), "dtc failed:\n{src}");
    let base = format!(
        "{}/../../tests/interfaces/dtb/{dtb}",
        env!("CARGO_MANIFEST_DIR")
    );
    let out = d.path().join("out.dtb");
    let ok = Command::new("fdtoverlay")
        .arg("-i")
        .arg(&base)
        .arg("-o")
        .arg(&out)
        .arg(&dtbo)
        .status()
        .unwrap();
    assert!(ok.success(), "fdtoverlay failed:\n{src}");
    let dts = Command::new("dtc")
        .args(["-q", "-I", "dtb", "-O", "dts"])
        .arg(&out)
        .output()
        .unwrap();
    Some(String::from_utf8_lossy(&dts.stdout).into_owned())
}

fn block<'a>(dts: &'a str, start: &str) -> &'a str {
    let i = dts
        .find(start)
        .unwrap_or_else(|| panic!("{start} not in merged tree"));
    let j = dts[i..]
        .find("\n\t\t};\n")
        .or_else(|| dts[i..].find("\n\t};\n"))
        .unwrap();
    &dts[i..i + j]
}

#[test]
fn rock4d_connector_applies_to_the_dtb() {
    let f = rock4d();
    with_interfaces(&f, "radxa-rock-4d");
    let (text, src) = enable(&f, None, fan::DEFAULT_CURVE);
    assert!(text.contains("Drive a fan on the fan connector"), "{text}");
    assert!(
        text.contains(
            "$ dtc -@ -q -I dts -O dtb -o /run/hwtool/hwtool-fan.dtbo /etc/hwtool/fan.dtso"
        ),
        "{text}"
    );
    assert!(text.contains("A reboot is needed"), "{text}");
    assert!(f
        .read("/boot/armbianEnv.txt")
        .contains("user_overlays=hwtool-fan\n"));
    assert!(src.contains("pwms = <&pwm2_8ch_5 0 60000 0>;"), "{src}");
    if let Some(dts) = apply(&src, "rk3576-rock-4d.dtb") {
        let fanb = block(&dts, "hwtool-fan {");
        assert!(fanb.contains("compatible = \"pwm-fan\""), "{fanb}");
        assert!(
            fanb.contains("cooling-levels = <0x00 0x4d 0x80 0xcc 0xff>"),
            "{fanb}"
        );
        let zone = block(&dts, "package-thermal {");
        assert!(zone.contains("polling-delay = <0x3e8>"), "{zone}");
        for t in ["0xc350", "0xea60", "0x11170", "0x13880"] {
            assert!(dts.contains(&format!("temperature = <{t}>")), "{t}");
        }
        assert!(dts.contains("hwtool-fan-map4"));
        assert!(
            dts.contains("package-crit"),
            "the existing critical trip stays"
        );
        let pwm = block(&dts, "pwm@2ade5000 {");
        assert!(pwm.contains("status = \"okay\""), "{pwm}");
    }
}

#[test]
fn cm5_connector_takes_pin_28_and_pwm2_ch6() {
    let f = cm5();
    with_interfaces(&f, "armsom-cm5-io");
    let (_, src) = enable(&f, None, "55:40,65:100");
    assert!(
        src.contains("pwms = <&pwm2_8ch_6 0 50000 0>;") && src.contains("&pwm2m2_ch6"),
        "{src}"
    );
    if let Some(dts) = apply(&src, "rk3576-armsom-cm5-io.dtb") {
        assert!(block(&dts, "pwm@2ade6000 {").contains("status = \"okay\""));
    }
    let st = interfaces::status(&f.root()).unwrap();
    assert!(
        st.busy.iter().any(|(p, w)| *p == 28 && w.contains("fan")),
        "{:?}",
        st.busy
    );
    let ch6 = st
        .interfaces
        .iter()
        .find(|e| e.interface.name == "pwm2-ch6-m3")
        .unwrap();
    assert_eq!(ch6.blocked_by, vec!["the fan".to_string()]);
    let err = interfaces::enable_plan(&f.root(), &["pwm2-ch6-m3".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("the fan"), "{err}");

    let p = fan::disable_plan(&f.root()).unwrap();
    assert!(
        p.render()
            .contains("rm -f /boot/overlay-user/hwtool-fan.dtbo /etc/hwtool/fan.json"),
        "{}",
        p.render()
    );
    p.execute(&f.root(), &mut RecordingExecutor::default(), "D")
        .unwrap();
    assert!(!f.read("/boot/armbianEnv.txt").contains("hwtool-fan"));
}

#[test]
fn header_pwm_and_refusals() {
    let f = rock4d();
    with_interfaces(&f, "radxa-rock-4d");
    f.programs(&["dtc"]);
    let mut ex = RecordingExecutor::default();
    assert!(fan::enable_plan(&f.root(), &mut ex, None, "50:30,40:50").is_err());
    assert!(fan::enable_plan(&f.root(), &mut ex, Some("i2c8-m2"), fan::DEFAULT_CURVE).is_err());
    assert!(fan::enable_plan(&f.root(), &mut ex, Some("nope"), fan::DEFAULT_CURVE).is_err());
    // A header PWM function that is already on as a plain output.
    f.write("/boot/overlay-user/hwtool-pwm1-ch3-m1.dtbo", "x");
    f.write(
        "/boot/armbianEnv.txt",
        &format!("{ROCK4D_ENV}user_overlays=hwtool-pwm1-ch3-m1\n"),
    );
    let err = fan::enable_plan(&f.root(), &mut ex, Some("pwm1-ch3-m1"), fan::DEFAULT_CURVE)
        .unwrap_err()
        .to_string();
    assert!(err.contains("disable it"), "{err}");
    f.write("/boot/armbianEnv.txt", ROCK4D_ENV);
    let (_, src) = enable(&f, Some("pwm1-ch3-m1"), "60:100");
    assert!(src.contains("pwms = <&pwm1_6ch_3 0 40000 0>;"), "{src}");
    if let Some(dts) = apply(&src, "rk3576-rock-4d.dtb") {
        assert!(block(&dts, "pwm@2add3000 {").contains("status = \"okay\""));
    }
    let st = interfaces::status(&f.root()).unwrap();
    assert!(st.busy.iter().any(|(p, _)| *p == 35));
}

#[test]
fn set_auto_and_show() {
    let f = rock4d();
    let z = "/sys/class/thermal/thermal_zone0";
    f.write(&format!("{z}/type"), "package-thermal\n");
    f.write(&format!("{z}/temp"), "52000\n");
    f.write(&format!("{z}/policy"), "step_wise\n");
    assert!(fan::set_plan(&f.root(), 50).is_err(), "no pwm-fan yet");
    f.write("/sys/class/hwmon/hwmon3/name", "pwmfan\n");
    f.write("/sys/class/hwmon/hwmon3/pwm1", "77\n");
    let c = "/sys/class/thermal/cooling_device5";
    f.write(&format!("{c}/type"), "pwm-fan\n");
    f.write(&format!("{c}/cur_state"), "1\n");
    f.write(&format!("{c}/max_state"), "4\n");
    let st = thermal::status(&f.root()).unwrap();
    assert!(st.fan);
    assert_eq!(st.fan_pwm, Some(77));
    assert!(fan::set_plan(&f.root(), 101).is_err());
    fan::set_plan(&f.root(), 100)
        .unwrap()
        .execute(&f.root(), &mut RecordingExecutor::default(), "S")
        .unwrap();
    assert_eq!(f.read(&format!("{z}/policy")), "user_space");
    assert_eq!(f.read("/sys/class/hwmon/hwmon3/pwm1"), "255");
    assert!(thermal::status(&f.root())
        .unwrap()
        .notes
        .iter()
        .any(|n| n.contains("fan auto")));
    fan::auto_plan(&f.root())
        .unwrap()
        .execute(&f.root(), &mut RecordingExecutor::default(), "A")
        .unwrap();
    assert_eq!(f.read(&format!("{z}/policy")), "step_wise");
}
