//! Boot options in /boot/armbianEnv.txt that boot.scr reads.
//!
//! Only keys the board's /boot/boot.cmd actually uses can be set, each with
//! its own check. Keys other commands own (rootdev, rootfstype, fdtfile,
//! overlays, overlay_prefix, user_overlays) are refused.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::env::BootEnv;
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::ENV_FILE;

pub const BOOT_CMD: &str = "/boot/boot.cmd";

struct Key {
    name: &'static str,
    help: &'static str,
    check: fn(&str) -> Result<()>,
}

fn one_of(v: &str, allowed: &[&str]) -> Result<()> {
    if allowed.contains(&v) {
        Ok(())
    } else {
        bail!("must be one of {}", allowed.join(", "))
    }
}

const KEYS: &[Key] = &[
    Key {
        name: "verbosity",
        help: "kernel log level on the console, 0 (quiet) to 7 (everything)",
        check: |v| one_of(v, &["0", "1", "2", "3", "4", "5", "6", "7"]),
    },
    Key {
        name: "console",
        help: "where the kernel console goes: serial, display or both",
        check: |v| one_of(v, &["serial", "display", "both"]),
    },
    Key {
        name: "bootlogo",
        help: "boot splash: true or false",
        check: |v| one_of(v, &["true", "false"]),
    },
    Key {
        name: "earlycon",
        help: "early console output: on or off",
        check: |v| one_of(v, &["on", "off"]),
    },
    Key {
        name: "docker_optimizations",
        help: "cgroup memory and cpuset options on the command line: on or off",
        check: |v| one_of(v, &["on", "off"]),
    },
    Key {
        name: "extraargs",
        help: "extra kernel command line arguments",
        check: check_extraargs,
    },
    Key {
        name: "usbstoragequirks",
        help: "usb-storage quirks, VID:PID:FLAGS separated by commas",
        check: check_quirks,
    },
];

const OWNED: &[(&str, &str)] = &[
    ("rootdev", "hwtool boot migrate"),
    ("rootfstype", "hwtool boot migrate"),
    ("fdtfile", "the U-Boot and device tree packages"),
    ("overlays", "hwtool overlays"),
    ("user_overlays", "hwtool overlays"),
    ("overlay_prefix", "hwtool kernel switch"),
];

fn check_extraargs(v: &str) -> Result<()> {
    if v.len() > 512 {
        bail!("is longer than 512 characters");
    }
    if let Some(c) = v.chars().find(|c| {
        (!c.is_ascii_graphic() && *c != ' ') || matches!(c, '"' | '\'' | '\\' | ';' | '$' | '`')
    }) {
        bail!("may not contain {c:?} (quotes, backslashes, ';', '$', '`' and control characters are refused)");
    }
    Ok(())
}

fn check_quirks(v: &str) -> Result<()> {
    for q in v.split(',') {
        let f: Vec<&str> = q.split(':').collect();
        let hex4 = |s: &str| s.len() == 4 && s.chars().all(|c| c.is_ascii_hexdigit());
        if f.len() != 3
            || !hex4(f[0])
            || !hex4(f[1])
            || f[2].is_empty()
            || !f[2].chars().all(|c| c.is_ascii_alphabetic())
        {
            bail!("'{q}' is not VID:PID:FLAGS (for example 0bda:9210:u)");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BootOption {
    pub key: String,
    pub help: String,
    /// Value in armbianEnv.txt, if set there.
    pub value: Option<String>,
    /// Default from boot.cmd, used when the key is not set.
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub options: Vec<BootOption>,
    /// Kernel command line as boot.scr builds it, if hwtool can follow it.
    pub cmdline: Option<String>,
}

fn boot_cmd(root: &Root) -> Result<String> {
    root.read_opt(BOOT_CMD).ok_or_else(|| {
        anyhow::anyhow!("{BOOT_CMD} is missing; the boot options cannot be checked against it")
    })
}

/// `setenv key "value"` defaults in boot.cmd.
fn defaults(cmd: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for l in cmd.lines() {
        if let Some(rest) = l.trim().strip_prefix("setenv ") {
            if let Some((k, v)) = rest.split_once(' ') {
                let v = v.trim();
                if v.starts_with('"') && v.ends_with('"') && !v[1..v.len() - 1].contains('$') {
                    out.entry(k.to_string())
                        .or_insert_with(|| v.trim_matches('"').to_string());
                }
            }
        }
    }
    out
}

fn uses(cmd: &str, key: &str) -> bool {
    cmd.contains(&format!("${{{key}}}"))
}

pub fn status(root: &Root) -> Result<Status> {
    let cmd = boot_cmd(root)?;
    let env = BootEnv::parse(&root.read(ENV_FILE)?);
    let defs = defaults(&cmd);
    let options = KEYS
        .iter()
        .filter(|k| uses(&cmd, k.name))
        .map(|k| BootOption {
            key: k.name.into(),
            help: k.help.into(),
            value: env.get(k.name),
            default: defs.get(k.name).cloned(),
        })
        .collect();
    Ok(Status {
        options,
        cmdline: cmdline(&cmd, &env),
    })
}

/// Follow boot-rk3576.cmd (and the other rockchip boot.cmd files of the same
/// shape) to the kernel command line. None if boot.cmd looks different.
pub fn cmdline(cmd: &str, env: &BootEnv) -> Option<String> {
    let defs = defaults(cmd);
    let get = |k: &str| {
        env.get(k)
            .or_else(|| defs.get(k).cloned())
            .unwrap_or_default()
    };
    let template = cmd.lines().find_map(|l| {
        let t = l.trim();
        t.strip_prefix("setenv bootargs \"")
            .and_then(|r| r.strip_suffix('"'))
            .filter(|r| r.starts_with("root="))
    })?;
    // The serial console argument from the line that handles "serial".
    let serial = cmd
        .lines()
        .find(|l| l.contains("\"${console}\" = \"serial\"") && l.contains("consoleargs"))
        .and_then(|l| l.split("setenv consoleargs \"").nth(1))
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("console=ttyS0,1500000")
        .to_string();
    let console = get("console");
    let mut consoleargs = String::new();
    if console == "display" || console == "both" {
        consoleargs = "console=tty1".into();
    }
    if console == "serial" || console == "both" {
        consoleargs = format!("{serial} {consoleargs}");
    }
    if get("earlycon") == "on" {
        consoleargs = format!("earlycon {consoleargs}");
    }
    consoleargs = if get("bootlogo") == "true" {
        format!("splash plymouth.ignore-serial-consoles {consoleargs}")
    } else {
        format!("splash=verbose {consoleargs}")
    };
    let mut out = template.to_string();
    let mut vars: BTreeMap<&str, String> = BTreeMap::new();
    vars.insert("consoleargs", consoleargs);
    vars.insert("partuuid", "<boot partition PARTUUID>".into());
    for k in [
        "rootdev",
        "rootfstype",
        "verbosity",
        "usbstoragequirks",
        "extraargs",
        "extraboardargs",
    ] {
        vars.insert(k, get(k));
    }
    for (k, v) in &vars {
        out = out.replace(&format!("${{{k}}}"), v);
    }
    if out.contains("${") {
        return None;
    }
    if get("docker_optimizations") == "on" && cmd.contains("docker_optimizations") {
        out.push_str(" cgroup_enable=cpuset cgroup_memory=1 cgroup_enable=memory");
    }
    Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn check_key(cmd: &str, key: &str) -> Result<&'static Key> {
    if let Some((_, owner)) = OWNED.iter().find(|(k, _)| *k == key) {
        bail!("{key} is changed by {owner}, not by hwtool bootenv");
    }
    let k = KEYS.iter().find(|k| k.name == key).ok_or_else(|| {
        anyhow::anyhow!(
            "hwtool bootenv does not set {key}; it sets {}",
            KEYS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
        )
    })?;
    if !uses(cmd, key) {
        bail!("this board's {BOOT_CMD} does not read {key}");
    }
    Ok(k)
}

fn env_plan(title: String, before: &BootEnv, after: &BootEnv, cmd: &str) -> Plan {
    let mut p = Plan::new(title);
    if before != after {
        p.step(
            format!("Back up {ENV_FILE}"),
            Action::Backup {
                path: ENV_FILE.into(),
            },
        );
        p.step(
            format!("Write the new {ENV_FILE}"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: after.render(),
            },
        );
        if let Some(c) = cmdline(cmd, after) {
            p.notices
                .push(format!("Kernel command line after the reboot: {c}"));
        }
        p.reboot_required = true;
    } else {
        p.notices
            .push("The value is already set; nothing to change.".into());
    }
    p
}

pub fn set_plan(root: &Root, assignment: &str) -> Result<Plan> {
    let (key, value) = assignment
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("expected KEY=VALUE, got {assignment:?}"))?;
    let cmd = boot_cmd(root)?;
    let k = check_key(&cmd, key)?;
    (k.check)(value).map_err(|e| anyhow::anyhow!("{key}: {e}"))?;
    let before = BootEnv::parse(&root.read(ENV_FILE)?);
    let mut after = before.clone();
    after.set(key, value);
    Ok(env_plan(
        format!("Set {key}={value} in {ENV_FILE}"),
        &before,
        &after,
        &cmd,
    ))
}

pub fn unset_plan(root: &Root, key: &str) -> Result<Plan> {
    let cmd = boot_cmd(root)?;
    check_key(&cmd, key)?;
    let before = BootEnv::parse(&root.read(ENV_FILE)?);
    if before.get(key).is_none() {
        bail!("{key} is not set in {ENV_FILE}");
    }
    let mut after = before.clone();
    after.remove(key);
    let mut p = env_plan(
        format!("Remove {key} from {ENV_FILE}"),
        &before,
        &after,
        &cmd,
    );
    if let Some(d) = defaults(&cmd).get(key) {
        p.notices
            .push(format!("boot.scr then uses its default, {key}={d}."));
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraargs_checks() {
        assert!(check_extraargs("usbcore.autosuspend=-1 quiet").is_ok());
        for bad in ["a\"b", "a'b", "a;b", "a$b", "a\nb", "a\\b", "a`b"] {
            assert!(check_extraargs(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn quirks() {
        assert!(check_quirks("0bda:9210:u,152d:0578:u").is_ok());
        assert!(check_quirks("0bda:9210").is_err());
        assert!(check_quirks("0bda:921:u").is_err());
    }
}
