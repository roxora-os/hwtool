//! LEDs in /sys/class/leds and their triggers.
//!
//! The board support package's armbian-led-state.service saves every LED's
//! trigger and writable settings to /etc/armbian-leds.conf when the system
//! shuts down and restores them at boot. hwtool does not add a second
//! mechanism: it sets the trigger now and writes it into that file's section
//! for the LED, so the setting also survives a power cut before the next
//! clean shutdown. Without that service, the setting is kept in
//! /etc/hwtool/leds.conf and applied at boot by hwtool-boot.service.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::plan::{Action, Plan};
use crate::root::Root;

pub const LEDS: &str = "/sys/class/leds";
pub const ARMBIAN_STATE: &str = "/etc/armbian-leds.conf";
pub const CONF: &str = "/etc/hwtool/leds.conf";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Led {
    pub name: String,
    pub trigger: String,
    pub triggers: Vec<String>,
    pub brightness: u32,
    pub max_brightness: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub leds: Vec<Led>,
    /// armbian-led-state.service is installed and keeps LED settings.
    pub armbian_led_state: bool,
}

pub fn armbian_led_state(root: &Root) -> bool {
    [
        "/lib/systemd/system",
        "/usr/lib/systemd/system",
        "/etc/systemd/system",
    ]
    .iter()
    .any(|d| root.exists(&format!("{d}/armbian-led-state.service")))
}

fn read(root: &Root, p: &str) -> String {
    root.read_opt(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn status(root: &Root) -> Result<Status> {
    let mut leds = Vec::new();
    for name in root.list_dir(LEDS) {
        let b = format!("{LEDS}/{name}");
        let t = read(root, &format!("{b}/trigger"));
        let mut trigger = String::new();
        let triggers = t
            .split_whitespace()
            .map(|w| {
                if let Some(x) = w.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                    trigger = x.to_string();
                    x.to_string()
                } else {
                    w.to_string()
                }
            })
            .collect();
        leds.push(Led {
            brightness: read(root, &format!("{b}/brightness")).parse().unwrap_or(0),
            max_brightness: read(root, &format!("{b}/max_brightness"))
                .parse()
                .unwrap_or(0),
            trigger,
            triggers,
            name,
        });
    }
    if leds.is_empty() {
        bail!("{LEDS} has no LEDs");
    }
    Ok(Status {
        leds,
        armbian_led_state: armbian_led_state(root),
    })
}

/// Set `trigger=` in the [<led path>] section of armbian-led-state's file,
/// keeping everything else. A missing section is appended.
pub fn set_in_state(text: &str, led_path: &str, trigger: &str) -> String {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut done = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section && !done {
                out.push(format!("trigger={trigger}"));
                done = true;
            }
            in_section = t == format!("[{led_path}]");
            out.push(line.to_string());
            continue;
        }
        if in_section && t.starts_with("trigger=") {
            if !done {
                out.push(format!("trigger={trigger}"));
                done = true;
            }
            continue;
        }
        out.push(line.to_string());
    }
    if in_section && !done {
        out.push(format!("trigger={trigger}"));
        done = true;
    }
    if !done {
        if !out.is_empty() && !out.last().is_some_and(|l| l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(format!("[{led_path}]"));
        out.push(format!("trigger={trigger}"));
    }
    out.join("\n") + "\n"
}

pub fn set_plan(root: &Root, name: &str, trigger: &str) -> Result<Plan> {
    let st = status(root)?;
    let led = st.leds.iter().find(|l| l.name == name).with_context(|| {
        format!(
            "no LED {name}; there are {}",
            st.leds
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    if !led.triggers.iter().any(|t| t == trigger) {
        bail!(
            "{name} offers the triggers {}, not {trigger}",
            led.triggers.join(" ")
        );
    }
    let path = format!("{LEDS}/{name}");
    let mut p = Plan::new(format!("Set the {name} LED to {trigger}"));
    p.step(
        format!("Set its trigger to {trigger}"),
        Action::SysfsWrite {
            path: format!("{path}/trigger"),
            value: trigger.into(),
        },
    );
    if st.armbian_led_state {
        let text = root.read_opt(ARMBIAN_STATE).unwrap_or_default();
        p.step(
            format!("Back up {ARMBIAN_STATE}"),
            Action::Backup {
                path: ARMBIAN_STATE.into(),
            },
        );
        p.step(
            format!("Keep it in {ARMBIAN_STATE}, which armbian-led-state.service restores at boot"),
            Action::WriteFile {
                path: ARMBIAN_STATE.into(),
                content: set_in_state(&text, &path, trigger),
            },
        );
    } else {
        let text = root.read_opt(CONF).unwrap_or_default();
        p.step(
            format!("Back up {CONF}"),
            Action::Backup { path: CONF.into() },
        );
        p.step(
            format!("Keep it in {CONF}"),
            Action::WriteFile {
                path: CONF.into(),
                content: set_in_state(&text, &path, trigger),
            },
        );
        crate::cpu::ensure_boot_unit(root, &mut p);
    }
    Ok(p)
}

/// Apply /etc/hwtool/leds.conf (only used without armbian-led-state).
pub fn apply_steps(root: &Root, p: &mut Plan) {
    let text = root.read_opt(CONF).unwrap_or_default();
    let mut led = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(x) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            led = Some(x.to_string());
        } else if let (Some(l), Some(v)) = (&led, t.strip_prefix("trigger=")) {
            if root.exists(&format!("{l}/trigger")) {
                p.step(
                    format!("Set {l} to {v}"),
                    Action::SysfsWrite {
                        path: format!("{l}/trigger"),
                        value: v.into(),
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_edits() {
        let text = "[/sys/class/leds/blue:status]\ntrigger=heartbeat\nbrightness=1\n\n[/sys/class/leds/green:power]\ntrigger=none\n";
        let out = set_in_state(text, "/sys/class/leds/blue:status", "none");
        assert_eq!(out, "[/sys/class/leds/blue:status]\ntrigger=none\nbrightness=1\n\n[/sys/class/leds/green:power]\ntrigger=none\n");
        let out = set_in_state("", "/sys/class/leds/x", "timer");
        assert_eq!(out, "[/sys/class/leds/x]\ntrigger=timer\n");
        let out = set_in_state(
            "[/sys/class/leds/y]\nbrightness=1\n",
            "/sys/class/leds/y",
            "timer",
        );
        assert_eq!(out, "[/sys/class/leds/y]\nbrightness=1\ntrigger=timer\n");
    }
}
