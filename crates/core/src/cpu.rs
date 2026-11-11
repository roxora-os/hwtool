//! CPU frequency per cpufreq policy (a cluster of cores: on RK3576 four
//! Cortex-A72 and four Cortex-A53).
//!
//! Settings are kept in /etc/hwtool/cpufreq.conf and applied at boot by
//! hwtool-cpufreq.service (`hwtool cpu apply`). Roxora images do not ship
//! Armbian's cpufrequtils defaults or armbian-hardware-optimize, so nothing
//! else sets the governor.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::plan::{Action, Plan};
use crate::root::Root;

pub const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
pub const CONF: &str = "/etc/hwtool/cpufreq.conf";
pub const UNIT: &str = "/etc/systemd/system/hwtool-cpufreq.service";
pub const UNIT_NAME: &str = "hwtool-cpufreq.service";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Policy {
    /// `policy0`, `policy4`, ...
    pub name: String,
    pub cpus: Vec<u32>,
    /// For example `Cortex-A72`, from the CPU part in /proc/cpuinfo.
    pub core: String,
    pub governor: String,
    pub governors: Vec<String>,
    pub min_khz: u64,
    pub max_khz: u64,
    /// Frequencies the policy offers, lowest first.
    pub frequencies_khz: Vec<u64>,
    /// What /etc/hwtool/cpufreq.conf sets for this policy, if anything.
    pub saved: Option<Setting>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Setting {
    pub governor: Option<String>,
    pub min_khz: Option<u64>,
    pub max_khz: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub policies: Vec<Policy>,
    /// /etc/hwtool/cpufreq.conf exists and the unit applies it at boot.
    pub persistent: bool,
}

fn core_name(part: &str) -> &'static str {
    match part {
        "0xd03" => "Cortex-A53",
        "0xd04" => "Cortex-A35",
        "0xd05" => "Cortex-A55",
        "0xd07" => "Cortex-A57",
        "0xd08" => "Cortex-A72",
        "0xd09" => "Cortex-A73",
        "0xd0a" => "Cortex-A75",
        "0xd0b" => "Cortex-A76",
        "0xd0d" => "Cortex-A77",
        _ => "",
    }
}

/// CPU number -> core name, from /proc/cpuinfo.
fn cores(root: &Root) -> Vec<(u32, String)> {
    let text = root.read_opt("/proc/cpuinfo").unwrap_or_default();
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let field = |k: &str| {
            block.lines().find_map(|l| {
                let (a, b) = l.split_once(':')?;
                (a.trim() == k).then(|| b.trim().to_string())
            })
        };
        if let (Some(n), Some(part)) = (
            field("processor").and_then(|n| n.parse().ok()),
            field("CPU part"),
        ) {
            out.push((n, core_name(&part).to_string()));
        }
    }
    out
}

fn read(root: &Root, p: &str) -> String {
    root.read_opt(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn num(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

pub fn parse_conf(text: &str) -> Vec<(String, Setting)> {
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut words = l.split_whitespace();
        let Some(name) = words.next() else { continue };
        let mut s = Setting::default();
        for w in words {
            match w.split_once('=') {
                Some(("governor", v)) => s.governor = Some(v.to_string()),
                Some(("min", v)) => s.min_khz = v.parse().ok(),
                Some(("max", v)) => s.max_khz = v.parse().ok(),
                _ => {}
            }
        }
        out.push((name.to_string(), s));
    }
    out
}

fn render_conf(entries: &[(String, Setting)]) -> String {
    let mut s = String::from(
        "# Written by hwtool cpu set; applied at boot by hwtool-cpufreq.service.\n\
         # One line per cpufreq policy: governor=, min= and max= in kHz.\n",
    );
    for (name, e) in entries {
        s.push_str(name);
        if let Some(g) = &e.governor {
            s.push_str(&format!(" governor={g}"));
        }
        if let Some(m) = e.min_khz {
            s.push_str(&format!(" min={m}"));
        }
        if let Some(m) = e.max_khz {
            s.push_str(&format!(" max={m}"));
        }
        s.push('\n');
    }
    s
}

pub fn status(root: &Root) -> Result<Status> {
    let conf = parse_conf(&root.read_opt(CONF).unwrap_or_default());
    let cores = cores(root);
    let mut policies = Vec::new();
    for name in root.list_dir(CPUFREQ) {
        if !name.starts_with("policy") {
            continue;
        }
        let b = format!("{CPUFREQ}/{name}");
        let cpus: Vec<u32> = read(root, &format!("{b}/affected_cpus"))
            .split_whitespace()
            .filter_map(|c| c.parse().ok())
            .collect();
        let core = cpus
            .first()
            .and_then(|c| cores.iter().find(|(n, _)| n == c))
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        let mut frequencies_khz: Vec<u64> =
            read(root, &format!("{b}/scaling_available_frequencies"))
                .split_whitespace()
                .filter_map(|f| f.parse().ok())
                .collect();
        frequencies_khz.sort_unstable();
        policies.push(Policy {
            cpus,
            core,
            governor: read(root, &format!("{b}/scaling_governor")),
            governors: read(root, &format!("{b}/scaling_available_governors"))
                .split_whitespace()
                .map(str::to_string)
                .collect(),
            min_khz: num(&read(root, &format!("{b}/scaling_min_freq"))),
            max_khz: num(&read(root, &format!("{b}/scaling_max_freq"))),
            frequencies_khz,
            saved: conf
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, s)| s.clone()),
            name,
        });
    }
    if policies.is_empty() {
        bail!("{CPUFREQ} has no policies: this kernel does not scale the CPU frequency");
    }
    Ok(Status {
        policies,
        persistent: root.exists(CONF) && root.exists(UNIT),
    })
}

fn unit() -> String {
    format!(
        "# Written by hwtool. Applies {CONF} at boot.\n\
         [Unit]\n\
         Description=Apply hwtool CPU frequency settings\n\
         ConditionPathExists={CONF}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart=/usr/bin/hwtool cpu apply\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Steps that write one policy's settings to sysfs now. Max before min when
/// raising, min before max when lowering, so the pair is never inverted.
fn apply_steps(p: &mut Plan, pol: &Policy, s: &Setting) {
    let b = format!("{CPUFREQ}/{}", pol.name);
    if let Some(g) = &s.governor {
        p.step(
            format!("Set the {} governor to {g}", pol.name),
            Action::SysfsWrite {
                path: format!("{b}/scaling_governor"),
                value: g.clone(),
            },
        );
    }
    let min = s.min_khz;
    let max = s.max_khz;
    let raise = max.is_some_and(|m| m >= pol.max_khz);
    let mut writes = Vec::new();
    if let Some(m) = min {
        writes.push(("scaling_min_freq", m));
    }
    if let Some(m) = max {
        writes.push(("scaling_max_freq", m));
    }
    if raise {
        writes.reverse();
    }
    for (f, v) in writes {
        p.step(
            format!(
                "Set {} {} to {} MHz",
                pol.name,
                f.trim_start_matches("scaling_"),
                v / 1000
            ),
            Action::SysfsWrite {
                path: format!("{b}/{f}"),
                value: v.to_string(),
            },
        );
    }
}

pub fn set_plan(
    root: &Root,
    policy: &str,
    governor: Option<&str>,
    min_khz: Option<u64>,
    max_khz: Option<u64>,
) -> Result<Plan> {
    let st = status(root)?;
    let pol = st
        .policies
        .iter()
        .find(|p| p.name == policy)
        .with_context(|| {
            format!(
                "no cpufreq policy {policy}; there are {}",
                st.policies
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    if governor.is_none() && min_khz.is_none() && max_khz.is_none() {
        bail!("give --governor, --min or --max");
    }
    if let Some(g) = governor {
        if !pol.governors.iter().any(|x| x == g) {
            bail!(
                "{policy} offers the governors {}, not {g}",
                pol.governors.join(", ")
            );
        }
    }
    for f in [min_khz, max_khz].into_iter().flatten() {
        if !pol.frequencies_khz.is_empty() && !pol.frequencies_khz.contains(&f) {
            bail!(
                "{policy} offers {} kHz, not {f}",
                pol.frequencies_khz
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let mut saved = pol.saved.clone().unwrap_or_default();
    if let Some(g) = governor {
        saved.governor = Some(g.into());
    }
    if min_khz.is_some() {
        saved.min_khz = min_khz;
    }
    if max_khz.is_some() {
        saved.max_khz = max_khz;
    }
    let lo = saved.min_khz.unwrap_or(pol.min_khz);
    let hi = saved.max_khz.unwrap_or(pol.max_khz);
    if lo > hi {
        bail!(
            "the minimum ({} MHz) would be above the maximum ({} MHz)",
            lo / 1000,
            hi / 1000
        );
    }
    let mut conf = parse_conf(&root.read_opt(CONF).unwrap_or_default());
    match conf.iter_mut().find(|(n, _)| n == policy) {
        Some(e) => e.1 = saved.clone(),
        None => conf.push((policy.to_string(), saved.clone())),
    }
    let mut p = Plan::new(format!(
        "Set CPU frequency for {policy} ({} CPUs {})",
        if pol.core.is_empty() {
            "cores"
        } else {
            &pol.core
        },
        pol.cpus
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    ));
    p.step(
        format!("Back up {CONF}"),
        Action::Backup { path: CONF.into() },
    );
    p.step(
        format!("Write {CONF}"),
        Action::WriteFile {
            path: CONF.into(),
            content: render_conf(&conf),
        },
    );
    if !st.persistent {
        p.step(
            format!("Write {UNIT}, which applies it at boot"),
            Action::WriteFile {
                path: UNIT.into(),
                content: unit(),
            },
        );
        p.run("Reload systemd", &["systemctl", "daemon-reload"]);
        p.run(
            format!("Enable {UNIT_NAME}"),
            &["systemctl", "enable", UNIT_NAME],
        );
    }
    apply_steps(&mut p, pol, &saved);
    if saved.governor.as_deref() == Some("performance")
        || saved
            .min_khz
            .is_some_and(|m| pol.frequencies_khz.last().is_some_and(|top| m >= *top))
    {
        p.warnings.push(
            "The cores will run at their top frequency all the time. That makes the board \
             hotter; without a heatsink or fan it may throttle or become unstable."
                .into(),
        );
    }
    Ok(p)
}

/// Remove hwtool's CPU settings. The current values stay until a reboot.
pub fn reset_plan(root: &Root) -> Result<Plan> {
    let mut p = Plan::new("Remove hwtool's CPU frequency settings");
    if !root.exists(CONF) && !root.exists(UNIT) {
        p.notices
            .push("hwtool has no CPU frequency settings on this system.".into());
        return Ok(p);
    }
    if root.exists(CONF) {
        p.step(
            format!("Back up {CONF}"),
            Action::Backup { path: CONF.into() },
        );
    }
    if root.exists(UNIT) {
        p.run(
            format!("Disable {UNIT_NAME}"),
            &["systemctl", "disable", UNIT_NAME],
        );
    }
    p.run(
        format!("Remove {CONF} and {UNIT}"),
        &["rm", "-f", CONF, UNIT],
    );
    p.run("Reload systemd", &["systemctl", "daemon-reload"]);
    p.notices
        .push("The kernel's defaults apply again after the next reboot.".into());
    Ok(p)
}

/// Write the saved settings to sysfs (run at boot by the unit).
pub fn apply_plan(root: &Root) -> Result<Plan> {
    let st = status(root)?;
    let mut p = Plan::new(format!("Apply {CONF}"));
    for (name, s) in parse_conf(&root.read_opt(CONF).unwrap_or_default()) {
        match st.policies.iter().find(|x| x.name == name) {
            Some(pol) => apply_steps(&mut p, pol, &s),
            None => p.warnings.push(format!(
                "{CONF} names {name}, which this system does not have"
            )),
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_round_trip() {
        let c = parse_conf("# x\npolicy0 governor=schedutil min=408000\npolicy4 max=1800000\n");
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].1.governor.as_deref(), Some("schedutil"));
        assert_eq!(parse_conf(&render_conf(&c)), c);
    }
}
