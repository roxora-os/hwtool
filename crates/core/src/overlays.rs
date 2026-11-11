//! Device tree overlays, as `boot.scr` applies them
//! (config/bootscripts/boot-rk3576.cmd in armbian/build):
//!
//! - every name in `overlays=` is looked up as
//!   `/boot/dtb/rockchip/overlay/<prefix>-<name>.dtbo` for each prefix in
//!   `overlay_prefix=` (first match wins), then as `<name>.dtbo`;
//! - every name in `user_overlays=` is loaded from `/boot/overlay-user/<name>.dtbo`.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
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

/// Board compatibles from the running device tree.
fn board_compatibles(root: &Root) -> Vec<String> {
    for base in ["/sys/firmware/devicetree/base", "/proc/device-tree"] {
        if let Ok(b) = std::fs::read(root.path(&format!("{base}/compatible"))) {
            return b
                .split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
        }
    }
    Vec::new()
}

/// The compatible strings an overlay source declares at its root node.
pub fn overlay_compatibles(src: &str) -> Vec<String> {
    let mut depth = 0i32;
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if depth == 1 && t.starts_with("compatible") && t.contains('=') {
            out.extend(t.split('"').skip(1).step_by(2).map(str::to_string));
        }
        depth += t.matches('{').count() as i32 - t.matches('}').count() as i32;
    }
    out
}

fn fnv(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Compile a .dts or .dtso overlay, install it in /boot/overlay-user and
/// enable it through user_overlays= (what armbian-add-overlay does, without
/// the C preprocessor). Installs device-tree-compiler from Ubuntu first if
/// dtc is missing.
pub fn add_plan(root: &Root, exec: &mut dyn crate::exec::Executor, file: &str) -> Result<Plan> {
    let path = std::path::Path::new(file);
    let name = match (path.file_stem(), path.extension().and_then(|e| e.to_str())) {
        (Some(stem), Some("dts" | "dtso")) => stem.to_string_lossy().into_owned(),
        _ => bail!("{file}: an overlay source ends in .dts or .dtso"),
    };
    validate_name(&name)?;
    let data = std::fs::read(path).with_context(|| format!("cannot read {file}"))?;
    let src = String::from_utf8_lossy(&data);
    if !src.contains("/plugin/") {
        bail!("{file} is not an overlay: it has no /plugin/ line");
    }
    if src.lines().any(|l| l.trim_start().starts_with("#include")) {
        bail!(
            "{file} uses #include, which needs the kernel headers and the C preprocessor; \
             compile it with armbian-add-overlay, then enable it here"
        );
    }
    let dst = format!("{USER_OVERLAY_DIR}/{name}.dtbo");
    if root.exists(&dst) {
        bail!("{dst} already exists; remove it first with 'hwtool overlays remove {name}'");
    }
    let wanted = overlay_compatibles(&src);
    let board = board_compatibles(root);
    let mut notices = Vec::new();
    if !wanted.is_empty() {
        if board.is_empty() {
            notices.push(format!(
                "Could not read the board's compatible strings; not checked against {}.",
                wanted.join(", ")
            ));
        } else if !wanted.iter().any(|w| board.contains(w)) {
            bail!(
                "{file} is for {}, and this board is {}",
                wanted.join(", "),
                board.join(", ")
            );
        }
    }
    let before = load_env(root)?;
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    if !users.contains(&name) {
        users.push(name.clone());
    }
    env.set_list("user_overlays", &users);

    let mut p = Plan::new(format!("Add the user overlay {name} from {file}"));
    if !root.has_program("dtc") {
        let sim = vec![
            "apt-get".to_string(),
            "-s".into(),
            "-y".into(),
            "--no-install-recommends".into(),
            "install".into(),
            "device-tree-compiler".into(),
        ];
        let out = exec
            .capture(&sim)
            .context("apt cannot resolve device-tree-compiler")?;
        let removals: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("Remv ") || l.starts_with("Purg "))
            .collect();
        if !removals.is_empty() {
            bail!(
                "installing device-tree-compiler would also remove: {}",
                removals.join("; ")
            );
        }
        p.run(
            "Install the device tree compiler",
            &[
                "apt-get",
                "install",
                "-y",
                "--no-install-recommends",
                "device-tree-compiler",
            ],
        );
        p.steps[0].details = std::iter::once("apt resolves:".to_string())
            .chain(
                out.lines()
                    .filter(|l| l.starts_with("Inst "))
                    .map(|l| format!("  {l}")),
            )
            .collect();
    }
    let tmp = format!("/run/hwtool/{name}.dtbo");
    p.run("Make the work directory", &["mkdir", "-p", "/run/hwtool"]);
    p.run(
        format!("Compile {file} (content {})", fnv(&data)),
        &[
            "dtc", "-@", "-q", "-I", "dts", "-O", "dtb", "-o", &tmp, file,
        ],
    );
    let install_at = p.steps.len();
    p.run(
        format!("Install it as {dst}"),
        &["install", "-D", "-m", "0644", &tmp, &dst],
    );
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
    let last = p.steps.len() - 1;
    p.on_failure(
        install_at,
        last,
        format!("Remove {dst}"),
        Action::Run {
            argv: vec!["rm".into(), "-f".into(), dst.clone()],
        },
    );
    p.notices = notices;
    p.notices
        .push(format!("boot.scr will load {dst} at the next boot."));
    p.reboot_required = true;
    Ok(p)
}

/// Remove a user overlay: disable it and delete its file (after a backup).
pub fn remove_plan(root: &Root, name: &str) -> Result<Plan> {
    validate_name(name)?;
    let dst = format!("{USER_OVERLAY_DIR}/{name}.dtbo");
    if !root.exists(&dst) {
        bail!(
            "{dst} does not exist; only overlays in {USER_OVERLAY_DIR} can be removed \
             (kernel overlays can only be disabled)"
        );
    }
    let before = load_env(root)?;
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    users.retain(|n| n != name);
    env.set_list("user_overlays", &users);
    let mut p = Plan::new(format!("Remove the user overlay {name}"));
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
    p.step(
        format!("Back up {dst}"),
        Action::Backup { path: dst.clone() },
    );
    p.run(format!("Delete {dst}"), &["rm", "-f", &dst]);
    p.reboot_required = true;
    Ok(p)
}
