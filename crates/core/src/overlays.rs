//! Device tree overlays, as `boot.scr` applies them
//! (config/bootscripts/boot-rk3576.cmd in armbian/build):
//!
//! - every name in `overlays=` is looked up as
//!   `/boot/dtb/rockchip/overlay/<prefix>-<name>.dtbo` for each prefix in
//!   `overlay_prefix=` (first match wins), then as `<name>.dtbo`;
//! - every name in `user_overlays=` is loaded from `/boot/overlay-user/<name>.dtbo`.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::env::BootEnv;
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::ENV_FILE;

pub const OVERLAY_DIR: &str = "/boot/dtb/rockchip/overlay";
pub const USER_OVERLAY_DIR: &str = "/boot/overlay-user";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Overlay {
    pub name: String,
    /// Path of the .dtbo file boot.scr would load.
    pub file: String,
    /// `kernel` (from the linux-dtb package) or `user` (/boot/overlay-user).
    pub kind: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub env_file: String,
    pub overlay_prefix: Vec<String>,
    pub overlays: Vec<Overlay>,
    /// Names in overlays= or user_overlays= for which no file exists.
    /// boot.scr skips them.
    pub missing: Vec<String>,
}

fn load_env(root: &Root) -> Result<BootEnv> {
    if !root.exists(ENV_FILE) {
        bail!(
            "{ENV_FILE} does not exist; this system does not boot with boot.scr and armbianEnv.txt"
        );
    }
    Ok(BootEnv::parse(&root.read(ENV_FILE)?))
}

/// Kernel overlays boot.scr can find with these prefixes: name -> file.
pub fn available(root: &Root, prefixes: &[String]) -> BTreeMap<String, String> {
    let files = root.list_dir(OVERLAY_DIR);
    let mut out = BTreeMap::new();
    for prefix in prefixes {
        let lead = format!("{prefix}-");
        for f in &files {
            if let Some(name) = f.strip_prefix(&lead).and_then(|r| r.strip_suffix(".dtbo")) {
                out.entry(name.to_string())
                    .or_insert_with(|| format!("{OVERLAY_DIR}/{f}"));
            }
        }
    }
    out
}

fn user_available(root: &Root) -> BTreeMap<String, String> {
    root.list_dir(USER_OVERLAY_DIR)
        .into_iter()
        .filter_map(|f| {
            f.strip_suffix(".dtbo")
                .map(|n| (n.to_string(), format!("{USER_OVERLAY_DIR}/{f}")))
        })
        .collect()
}

/// The file boot.scr loads for a name in overlays=, if any.
fn resolve_kernel(root: &Root, prefixes: &[String], name: &str) -> Option<String> {
    for p in prefixes {
        let f = format!("{OVERLAY_DIR}/{p}-{name}.dtbo");
        if root.exists(&f) {
            return Some(f);
        }
    }
    let f = format!("{OVERLAY_DIR}/{name}.dtbo");
    root.exists(&f).then_some(f)
}

pub fn status(root: &Root) -> Result<Status> {
    let env = load_env(root)?;
    let prefixes = env.list("overlay_prefix");
    let enabled = env.list("overlays");
    let user_enabled = env.list("user_overlays");
    let mut overlays: Vec<Overlay> = available(root, &prefixes)
        .into_iter()
        .map(|(name, file)| Overlay {
            enabled: enabled.contains(&name),
            name,
            file,
            kind: "kernel".into(),
        })
        .collect();
    // Enabled by a full file name rather than by prefix.
    for name in &enabled {
        if !overlays
            .iter()
            .any(|o| o.kind == "kernel" && &o.name == name)
        {
            if let Some(file) = resolve_kernel(root, &prefixes, name) {
                overlays.push(Overlay {
                    name: name.clone(),
                    file,
                    kind: "kernel".into(),
                    enabled: true,
                });
            }
        }
    }
    for (name, file) in user_available(root) {
        overlays.push(Overlay {
            enabled: user_enabled.contains(&name),
            name,
            file,
            kind: "user".into(),
        });
    }
    let mut missing = Vec::new();
    for n in &enabled {
        if resolve_kernel(root, &prefixes, n).is_none() {
            missing.push(n.clone());
        }
    }
    for n in &user_enabled {
        if !root.exists(&format!("{USER_OVERLAY_DIR}/{n}.dtbo")) {
            missing.push(n.clone());
        }
    }
    Ok(Status {
        env_file: ENV_FILE.into(),
        overlay_prefix: prefixes,
        overlays,
        missing,
    })
}

fn env_plan(title: String, before: &BootEnv, after: &BootEnv) -> Plan {
    let mut plan = Plan::new(title);
    if before != after {
        plan.step(
            format!("Back up {ENV_FILE}"),
            Action::Backup {
                path: ENV_FILE.into(),
            },
        );
        plan.step(
            format!("Write the new {ENV_FILE}"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: after.render(),
            },
        );
        plan.reboot_required = true;
    }
    plan
}

/// Enable overlays. A name is looked up among the kernel overlays first
/// (added to overlays=), then in /boot/overlay-user (added to user_overlays=).
pub fn enable(root: &Root, names: &[String]) -> Result<Plan> {
    let before = load_env(root)?;
    let mut env = before.clone();
    let prefixes = env.list("overlay_prefix");
    let users = user_available(root);
    let mut kernel_list = env.list("overlays");
    let mut user_list = env.list("user_overlays");
    let mut notices = Vec::new();
    for name in names {
        validate_name(name)?;
        if let Some(file) = resolve_kernel(root, &prefixes, name) {
            if kernel_list.contains(name) {
                notices.push(format!("{name} is already enabled"));
            } else {
                kernel_list.push(name.clone());
                notices.push(format!("{name}: boot.scr will load {file}"));
            }
        } else if let Some(file) = users.get(name) {
            if user_list.contains(name) {
                notices.push(format!("{name} is already enabled"));
            } else {
                user_list.push(name.clone());
                notices.push(format!("{name}: boot.scr will load {file}"));
            }
        } else {
            let avail = available(root, &prefixes);
            let mut list: Vec<String> = avail.keys().cloned().collect();
            list.extend(users.keys().cloned());
            if list.is_empty() {
                bail!(
                    "no overlay named {name}: the installed device tree package has no overlays for \
                     this board (overlay_prefix={})",
                    prefixes.join(" ")
                );
            }
            bail!("no overlay named {name}; available: {}", list.join(", "));
        }
    }
    env.set_list("overlays", &kernel_list);
    env.set_list("user_overlays", &user_list);
    let mut plan = env_plan(
        format!("Enable overlays: {}", names.join(" ")),
        &before,
        &env,
    );
    plan.notices = notices;
    Ok(plan)
}

pub fn disable(root: &Root, names: &[String]) -> Result<Plan> {
    let before = load_env(root)?;
    let mut env = before.clone();
    let mut kernel_list = env.list("overlays");
    let mut user_list = env.list("user_overlays");
    for name in names {
        validate_name(name)?;
        let k = kernel_list.len() + user_list.len();
        kernel_list.retain(|n| n != name);
        user_list.retain(|n| n != name);
        if k == kernel_list.len() + user_list.len() {
            bail!("{name} is not enabled");
        }
    }
    env.set_list("overlays", &kernel_list);
    env.set_list("user_overlays", &user_list);
    Ok(env_plan(
        format!("Disable overlays: {}", names.join(" ")),
        &before,
        &env,
    ))
}

/// Overlay names are space-separated in armbianEnv.txt and become file names.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name
            .chars()
            .any(|c| c.is_whitespace() || c == '=' || c.is_control())
        || name.starts_with('.')
        || name.starts_with('-')
    {
        bail!("invalid overlay name {name:?}");
    }
    Ok(())
}
