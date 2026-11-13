//! PWM fan control through a `pwm-fan` device tree node.
//!
//! `fan enable` writes a user overlay that adds a pwm-fan node on a PWM
//! channel, with `cooling-levels` from a temperature curve, and active trip
//! points with cooling maps on the SoC's `package-thermal` zone, so the
//! kernel's thermal governor drives the fan. Facts:
//!
//! - Radxa ROCK 4D: a 2-pin 1.25 mm 5 V fan connector (Radxa docs,
//!   rock4/rock4d/hardware-use/fan.md); Radxa's kernel drives it from
//!   `pwm2_8ch_5` with pin group `pwm2m1_ch5` (GPIO4_C5, not on the header)
//!   and a 60 us period (radxa/kernel linux-6.1-stan-rkr5.1,
//!   rk3576-rock-4d.dts).
//! - ArmSoM CM5-IO: a 3-pin 0.8 mm connector CN32 (5 V, GND, a return
//!   switched by Q45/Q46) whose control is `PWM2_CH6_M2`, shared with header
//!   pin 28 (ArmSoM/documentation cm5.md at 90a836e7d7da); ArmSoM's kernel
//!   uses `pwm2_8ch_6`, `pwm2m2_ch6`, 50 us (ArmSoM/rockchip-kernel
//!   e47316851307, rk3576-armsom-cm5-io.dts).
//! - Neither connector has a tachometer line, so there is no fan-tach.
//! - The zone, the PWM controllers and the pin groups are labels in the
//!   boards' DTBs (`package_thermal`, `pwm2_8ch_5`, `pwm2m1_ch5`, ...). The
//!   edge kernel has CONFIG_SENSORS_PWM_FAN=m, PWM_ROCKCHIP_V4=y and the
//!   user_space thermal governor (armbian/build
//!   config/kernel/linux-rockchip64-edge.config).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::env::BootEnv;
use crate::exec::Executor;
use crate::overlays::USER_OVERLAY_DIR;
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::ENV_FILE;

pub const CONF: &str = "/etc/hwtool/fan.json";
pub const SOURCE: &str = "/etc/hwtool/fan.dtso";
pub const OVERLAY: &str = "hwtool-fan";
pub const ZONE: &str = "package-thermal";
pub const HYSTERESIS_MC: u32 = 5000;
pub const DEFAULT_CURVE: &str = "50:30,60:50,70:80,80:100";

/// A board's own fan connector.
struct Connector {
    board: &'static str,
    compatible: &'static str,
    node: &'static str,
    group: &'static str,
    period_ns: u32,
    pins: &'static [u32],
    about: &'static str,
}

const CONNECTORS: &[Connector] = &[
    Connector {
        board: "radxa-rock-4d",
        compatible: "radxa,rock-4d",
        node: "pwm2_8ch_5",
        group: "pwm2m1_ch5",
        period_ns: 60000,
        pins: &[],
        about: "the board's 2-pin fan connector (PWM2 channel 5)",
    },
    Connector {
        board: "armsom-cm5-io",
        compatible: "armsom,cm5-io",
        node: "pwm2_8ch_6",
        group: "pwm2m2_ch6",
        period_ns: 50000,
        pins: &[28],
        about: "the carrier's fan connector CN32 (PWM2 channel 6, shared with header pin 28)",
    },
];

/// What `fan enable` set up, kept in /etc/hwtool/fan.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conf {
    /// "fan" for the board's connector, else a header PWM function.
    pub pwm: String,
    pub node: String,
    pub pins: Vec<u32>,
    pub curve: Vec<(u32, u32)>,
}

pub fn configured(root: &Root) -> Option<Conf> {
    serde_json::from_str(&root.read_opt(CONF)?).ok()
}

/// "50:30,60:50": temperatures in C ascending within 30..100, duty in
/// percent 0..100 not decreasing, 1 to 8 points.
pub fn parse_curve(s: &str) -> Result<Vec<(u32, u32)>> {
    let mut v: Vec<(u32, u32)> = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (t, d) = part.split_once(':').with_context(|| {
            format!("{part}: a point is TEMPERATURE:PERCENT, for example 60:50")
        })?;
        let t: u32 = t
            .trim()
            .parse()
            .with_context(|| format!("{part}: bad temperature"))?;
        let d: u32 = d
            .trim()
            .trim_end_matches('%')
            .parse()
            .with_context(|| format!("{part}: bad percentage"))?;
        if !(30..=100).contains(&t) {
            bail!("{part}: temperatures are 30 to 100 C");
        }
        if d > 100 {
            bail!("{part}: the fan speed is 0 to 100 %");
        }
        if let Some((pt, pd)) = v.last() {
            if t <= *pt {
                bail!("{part}: temperatures must go up from point to point");
            }
            if d < *pd {
                bail!("{part}: the fan speed must not go down as the temperature goes up");
            }
        }
        v.push((t, d));
    }
    if v.is_empty() || v.len() > 8 {
        bail!("a curve has 1 to 8 points, for example {DEFAULT_CURVE}");
    }
    Ok(v)
}

fn level(pct: u32) -> u32 {
    (pct * 255 + 50) / 100
}

/// The overlay source.
pub fn overlay(
    compatible: &str,
    node: &str,
    group: &str,
    period_ns: u32,
    curve: &[(u32, u32)],
) -> String {
    let levels: Vec<String> = std::iter::once("0".to_string())
        .chain(curve.iter().map(|(_, d)| level(*d).to_string()))
        .collect();
    let mut trips = String::new();
    let mut maps = String::new();
    for (i, (t, _)) in curve.iter().enumerate() {
        let k = i + 1;
        trips.push_str(&format!(
            "\t\thwtool_fan_trip{k}: hwtool-fan{k} {{\n\t\t\ttemperature = <{}>;\n\t\t\thysteresis = <{HYSTERESIS_MC}>;\n\t\t\ttype = \"active\";\n\t\t}};\n",
            t * 1000
        ));
        maps.push_str(&format!(
            "\t\thwtool-fan-map{k} {{\n\t\t\ttrip = <&hwtool_fan_trip{k}>;\n\t\t\tcooling-device = <&hwtool_fan {k} {k}>;\n\t\t}};\n"
        ));
    }
    format!(
        "// SPDX-License-Identifier: GPL-2.0-only\n\
         // Written by hwtool fan enable: a PWM fan driven by package-thermal.\n\
         /dts-v1/;\n/plugin/;\n\n\
         / {{\n\tcompatible = \"{compatible}\", \"rockchip,rk3576\";\n}};\n\n\
         &{{/}} {{\n\thwtool_fan: hwtool-fan {{\n\t\tcompatible = \"pwm-fan\";\n\t\t#cooling-cells = <2>;\n\
         \t\tpwms = <&{node} 0 {period_ns} 0>;\n\t\tcooling-levels = <{}>;\n\t}};\n}};\n\n\
         &{node} {{\n\tpinctrl-names = \"default\";\n\tpinctrl-0 = <&{group}>;\n\tstatus = \"okay\";\n}};\n\n\
         &package_thermal {{\n\tpolling-delay = <1000>;\n\n\ttrips {{\n{trips}\t}};\n\n\tcooling-maps {{\n{maps}\t}};\n}};\n",
        levels.join(" ")
    )
}

/// The PWM sources on this board: (name, description).
pub fn sources(root: &Root) -> Result<Vec<(String, String)>> {
    let board = Board::detect(root)?;
    let mut v = Vec::new();
    if let Some(c) = CONNECTORS.iter().find(|c| c.board == board.board) {
        v.push(("fan".to_string(), c.about.to_string()));
    }
    if let Ok(st) = crate::interfaces::status(root) {
        for e in st.interfaces.iter().filter(|e| e.interface.kind == "pwm") {
            let pins: Vec<String> = e.interface.pins.iter().map(|p| p.pin.to_string()).collect();
            v.push((
                e.interface.name.clone(),
                format!("{} on header pin {}", e.interface.title, pins.join(", ")),
            ));
        }
    }
    Ok(v)
}

fn load_env(root: &Root) -> Result<BootEnv> {
    Ok(BootEnv::parse(&root.read(ENV_FILE)?))
}

pub fn enable_plan(
    root: &Root,
    exec: &mut dyn Executor,
    pwm: Option<&str>,
    curve: &str,
) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::OVERLAYS)?;
    let board = Board::detect(root)?;
    let curve = parse_curve(curve)?;
    let connector = CONNECTORS.iter().find(|c| c.board == board.board);
    let pwm = match pwm {
        Some(p) => p.to_string(),
        None if connector.is_some() => "fan".to_string(),
        None => bail!(
            "{} has no fan connector hwtool knows; name a header PWM function with --pwm",
            board.board_name
        ),
    };
    let ifs = crate::interfaces::status(root)?;
    let (compatible, node, group, period, pins) = if pwm == "fan" {
        let c = connector
            .with_context(|| format!("{} has no fan connector hwtool knows", board.board_name))?;
        (
            c.compatible.to_string(),
            c.node.to_string(),
            c.group.to_string(),
            c.period_ns,
            c.pins.to_vec(),
        )
    } else {
        let e = ifs
            .interfaces
            .iter()
            .find(|e| e.interface.name == pwm && e.interface.kind == "pwm")
            .with_context(|| {
                format!(
                    "{pwm} is not a PWM function of this board; there are: {}",
                    sources(root)
                        .unwrap_or_default()
                        .iter()
                        .map(|s| s.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        let compatible = CONNECTORS
            .iter()
            .find(|c| c.board == board.board)
            .map(|c| c.compatible)
            .unwrap_or("rockchip,rk3576");
        (
            compatible.to_string(),
            e.interface.node.clone(),
            e.interface
                .groups
                .first()
                .cloned()
                .context("no pin group")?,
            // 25 kHz, the PWM frequency of 4-pin PC fans.
            40000,
            e.interface.pins.iter().map(|p| p.pin).collect(),
        )
    };
    // A header function on the same controller or pins cannot be on too.
    let clash: Vec<String> = ifs
        .interfaces
        .iter()
        .filter(|e| e.enabled && e.interface.name != pwm)
        .filter(|e| {
            e.interface.node == node || e.interface.pins.iter().any(|p| pins.contains(&p.pin))
        })
        .map(|e| e.interface.name.clone())
        .collect();
    if !clash.is_empty() {
        bail!(
            "{pwm} uses the pins or the PWM channel of {}, which is enabled; disable it first",
            clash.join(", ")
        );
    }
    if ifs
        .interfaces
        .iter()
        .any(|e| e.enabled && e.interface.name == pwm)
    {
        bail!("{pwm} is enabled as a plain PWM output; disable it with 'hwtool interfaces disable {pwm}' first");
    }
    let src = overlay(&compatible, &node, &group, period, &curve);
    let conf = Conf {
        pwm: pwm.clone(),
        node: node.clone(),
        pins: pins.clone(),
        curve: curve.clone(),
    };
    let dst = format!("{USER_OVERLAY_DIR}/{OVERLAY}.dtbo");
    let before = load_env(root)?;
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    if !users.contains(&OVERLAY.to_string()) {
        users.push(OVERLAY.into());
    }
    env.set_list("user_overlays", &users);

    let mut p = Plan::new(format!(
        "Drive a fan on {} from the SoC temperature: {}",
        if pwm == "fan" {
            "the fan connector".to_string()
        } else {
            pwm.clone()
        },
        curve
            .iter()
            .map(|(t, d)| format!("{d}% at {t} C"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    crate::overlays::ensure_dtc(root, exec, &mut p)?;
    p.step(
        format!("Write the overlay source {SOURCE}"),
        Action::WriteFile {
            path: SOURCE.into(),
            content: src,
        },
    );
    p.run("Make the work directory", &["mkdir", "-p", "/run/hwtool"]);
    let tmp = "/run/hwtool/hwtool-fan.dtbo";
    p.run(
        "Compile it",
        &[
            "dtc", "-@", "-q", "-I", "dts", "-O", "dtb", "-o", tmp, SOURCE,
        ],
    );
    let first = p.steps.len();
    p.run(
        format!("Install it as {dst}"),
        &["install", "-D", "-m", "0644", tmp, &dst],
    );
    p.step(
        format!("Write {CONF}"),
        Action::WriteFile {
            path: CONF.into(),
            content: serde_json::to_string_pretty(&conf)? + "\n",
        },
    );
    if env != before {
        p.step(
            format!("Back up {ENV_FILE}"),
            Action::Backup {
                path: ENV_FILE.into(),
            },
        );
        p.step(
            format!("Enable it in {ENV_FILE}"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: env.render(),
            },
        );
    }
    let last = p.steps.len() - 1;
    p.on_failure(
        first,
        last,
        format!("Remove {dst} and {CONF}"),
        Action::Run {
            argv: vec!["rm".into(), "-f".into(), dst.clone(), CONF.into()],
        },
    );
    p.notices.push(format!(
        "The fan starts at {} C and turns down again {} C below each step. Trip points are on {ZONE}.",
        curve[0].0,
        HYSTERESIS_MC / 1000
    ));
    if !pins.is_empty() {
        p.notices.push(format!(
            "Header pin {} is the fan's now and is not offered for other functions.",
            pins.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    p.reboot_required = true;
    Ok(p)
}

pub fn disable_plan(root: &Root) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::OVERLAYS)?;
    let before = load_env(root)?;
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    let mut p = Plan::new("Stop the kernel driving the fan");
    if !users.contains(&OVERLAY.to_string()) && !root.exists(CONF) {
        p.notices.push("hwtool has no fan set up.".into());
        return Ok(p);
    }
    users.retain(|u| u != OVERLAY);
    env.set_list("user_overlays", &users);
    if env != before {
        p.step(
            format!("Back up {ENV_FILE}"),
            Action::Backup {
                path: ENV_FILE.into(),
            },
        );
        p.step(
            format!("Disable it in {ENV_FILE}"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: env.render(),
            },
        );
    }
    p.run(
        "Delete the overlay and hwtool's fan settings",
        &[
            "rm",
            "-f",
            &format!("{USER_OVERLAY_DIR}/{OVERLAY}.dtbo"),
            CONF,
            SOURCE,
        ],
    );
    p.reboot_required = true;
    Ok(p)
}

/// The pwm-fan hwmon directory (name "pwmfan"), if the driver is bound.
pub fn hwmon(root: &Root) -> Option<String> {
    root.list_dir("/sys/class/hwmon")
        .into_iter()
        .map(|n| format!("/sys/class/hwmon/{n}"))
        .find(|d| {
            root.read_opt(&format!("{d}/name"))
                .is_some_and(|n| n.trim() == "pwmfan")
        })
}

fn zone_dir(root: &Root) -> Option<String> {
    root.list_dir(crate::thermal::THERMAL)
        .into_iter()
        .filter(|n| n.starts_with("thermal_zone"))
        .map(|n| format!("{}/{n}", crate::thermal::THERMAL))
        .find(|d| {
            root.read_opt(&format!("{d}/type"))
                .is_some_and(|t| t.trim() == ZONE)
        })
}

/// Run the fan at `pct` % until reboot or `fan auto`: the zone's governor
/// becomes user_space, so it does not change the speed back, and the
/// speed is written to the pwm-fan hwmon's pwm1. Critical trips still work.
pub fn set_plan(root: &Root, pct: u32) -> Result<Plan> {
    if pct > 100 {
        bail!("the fan speed is 0 to 100 %");
    }
    let hw = hwmon(root)
        .context("no pwm-fan device: enable a fan with 'hwtool fan enable' and reboot first")?;
    let zone = zone_dir(root).with_context(|| format!("no {ZONE} thermal zone"))?;
    let mut p = Plan::new(format!(
        "Run the fan at {pct}% until the next boot or 'hwtool fan auto'"
    ));
    p.step(
        format!("Stop the thermal governor changing it ({zone}/policy: user_space)"),
        Action::SysfsWrite {
            path: format!("{zone}/policy"),
            value: "user_space".into(),
        },
    );
    p.step(
        format!("Set the speed ({hw}/pwm1)"),
        Action::SysfsWrite {
            path: format!("{hw}/pwm1"),
            value: level(pct).to_string(),
        },
    );
    p.warnings.push(
        "The fan no longer follows the temperature until 'hwtool fan auto' or a reboot.".into(),
    );
    Ok(p)
}

pub fn auto_plan(root: &Root) -> Result<Plan> {
    let zone = zone_dir(root).with_context(|| format!("no {ZONE} thermal zone"))?;
    let mut p = Plan::new("Let the thermal governor drive the fan again");
    p.step(
        format!("{zone}/policy: step_wise"),
        Action::SysfsWrite {
            path: format!("{zone}/policy"),
            value: "step_wise".into(),
        },
    );
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves() {
        assert_eq!(
            parse_curve(DEFAULT_CURVE).unwrap(),
            vec![(50, 30), (60, 50), (70, 80), (80, 100)]
        );
        for bad in [
            "",
            "50",
            "20:30",
            "101:30",
            "50:101",
            "60:30,50:40",
            "50:50,60:40",
            "50:1,51:1,52:1,53:1,54:1,55:1,56:1,57:1,58:1",
        ] {
            assert!(parse_curve(bad).is_err(), "{bad}");
        }
        assert_eq!(level(100), 255);
        assert_eq!(level(50), 128);
    }

    #[test]
    fn overlay_text() {
        let o = overlay(
            "radxa,rock-4d",
            "pwm2_8ch_5",
            "pwm2m1_ch5",
            60000,
            &[(50, 30), (70, 100)],
        );
        assert!(o.contains("pwms = <&pwm2_8ch_5 0 60000 0>;"));
        assert!(o.contains("cooling-levels = <0 77 255>;"));
        assert!(o.contains("cooling-device = <&hwtool_fan 2 2>;"));
        assert!(o.contains("temperature = <70000>;"));
        assert!(o.contains("hysteresis = <5000>;"));
    }
}
