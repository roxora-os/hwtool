//! Switching between the kernel branches a board supports.
//!
//! A branch is three packages from the Armbian repository:
//! `linux-image-<branch>-<family>`, `linux-dtb-<branch>-<family>` and
//! `linux-u-boot-<board>-<branch>` (lib/functions/compilation/kernel-debs.sh
//! and uboot.sh in armbian/build). Kernel packages of different branches can
//! be installed side by side; the one installed last owns `/boot/Image` and
//! `/boot/dtb`. U-Boot packages conflict with each other (`armbian-u-boot`),
//! so apt replaces the old one.
//!
//! The switch installs the new branch, checks that `/boot/Image` and the
//! device tree are in place, then removes the old kernel packages and
//! records the old versions so `kernel revert` can go back.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::dpkg::{self, Package};
use crate::env::BootEnv;
use crate::exec::{argv, Executor};
use crate::plan::{self, Action, Plan};
use crate::root::Root;
use crate::ENV_FILE;

pub const HISTORY_FILE: &str = "/var/lib/hwtool/kernel-history.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub time: String,
    pub from_branch: Option<String>,
    pub from_packages: Vec<Package>,
    pub to_branch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchInfo {
    pub branch: String,
    pub packages: Vec<String>,
    pub installed: Vec<Package>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub board: Board,
    /// Branch of the kernel /boot/Image points at (the one that boots next).
    pub current: Option<String>,
    pub kernel: Option<String>,
    pub branches: Vec<BranchInfo>,
    pub history: Vec<HistoryEntry>,
}

pub fn read_history(root: &Root) -> Vec<HistoryEntry> {
    root.read_opt(HISTORY_FILE)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn branch_packages(board: &Board, branch: &str) -> Vec<String> {
    vec![
        board.image_package(branch),
        board.dtb_package(branch),
        board.uboot_package(branch),
    ]
}

/// The branch packages plus headers, for showing what is installed
/// (headers are optional, so they are not listed when missing).
fn shown_packages(board: &Board, branch: &str) -> Vec<String> {
    let mut v = branch_packages(board, branch);
    v.push(board.headers_package(branch));
    v
}

/// `vmlinuz-7.3.4-edge-rockchip64` -> `7.3.4-edge-rockchip64`.
fn next_kernel(root: &Root) -> Option<String> {
    let t = root.read_link("/boot/Image")?;
    let name = t.rsplit('/').next()?.to_string();
    Some(name.strip_prefix("vmlinuz-").unwrap_or(&name).to_string())
}

pub fn status(root: &Root) -> Result<Status> {
    let board = Board::detect(root)?;
    let installed = dpkg::installed(root);
    let kernel = next_kernel(root);
    let branches: Vec<BranchInfo> = board
        .branches
        .iter()
        .map(|b| {
            let shown = shown_packages(&board, b);
            BranchInfo {
                installed: installed
                    .iter()
                    .filter(|p| shown.contains(&p.name))
                    .cloned()
                    .collect(),
                packages: branch_packages(&board, b),
                branch: b.clone(),
            }
        })
        .collect();
    let current = kernel
        .as_ref()
        .and_then(|k| {
            board
                .branches
                .iter()
                .find(|b| k.ends_with(&format!("-{b}-{}", board.family_for(b))))
                .cloned()
        })
        .or_else(|| {
            let with_image: Vec<&BranchInfo> = branches
                .iter()
                .filter(|bi| {
                    bi.installed
                        .iter()
                        .any(|p| p.name == board.image_package(&bi.branch))
                })
                .collect();
            (with_image.len() == 1).then(|| with_image[0].branch.clone())
        });
    Ok(Status {
        history: read_history(root),
        board,
        current,
        kernel,
        branches,
    })
}

fn policy(exec: &mut dyn Executor, pkg: &str) -> Result<dpkg::Policy> {
    let out = exec
        .capture(&argv(&["apt-cache", "policy", pkg]))
        .with_context(|| format!("cannot ask apt about {pkg}"))?;
    Ok(dpkg::parse_policy(&out))
}

/// Plan a switch to `target`. `want` pins package versions (used by revert)
/// when apt still offers them.
pub fn switch_plan(
    root: &Root,
    exec: &mut dyn Executor,
    target: &str,
    keep_old: bool,
    want: &BTreeMap<String, String>,
) -> Result<Plan> {
    let st = status(root)?;
    let board = &st.board;
    if !board.supports(target) {
        bail!(
            "{} supports the kernel branches {}; {target} is not one of them",
            board.board_name,
            board.branches.join(", ")
        );
    }
    let installed = dpkg::installed(root);
    let mut new = branch_packages(board, target);
    // Headers are installed only where DKMS modules need them. When the
    // running branch has them, the new branch gets its own.
    let has_headers = board
        .branches
        .iter()
        .any(|b| installed.iter().any(|p| p.name == board.headers_package(b)));
    if has_headers {
        new.push(board.headers_package(target));
    }
    if st.current.as_deref() == Some(target)
        && new.iter().all(|n| installed.iter().any(|p| &p.name == n))
    {
        bail!(
            "this system already runs the {target} branch ({})",
            st.kernel.unwrap_or_default()
        );
    }

    let mut specs = Vec::new();
    let mut missing = Vec::new();
    for pkg in &new {
        let p = policy(exec, pkg)?;
        match (&p.candidate, want.get(pkg)) {
            (_, Some(v)) if p.versions.contains(v) => specs.push(format!("{pkg}={v}")),
            (Some(_), _) => specs.push(pkg.clone()),
            (None, _) => missing.push(pkg.clone()),
        }
    }
    if !missing.is_empty() {
        bail!(
            "apt has no installable version of {}. The Armbian repository may not publish this \
             branch for {}, or the package lists are out of date (run apt-get update)",
            missing.join(", "),
            board.board_name
        );
    }

    // Kernel, DTB and header packages of the other branches. Old U-Boot packages
    // are replaced by apt because they all conflict on armbian-u-boot.
    let old: Vec<Package> = installed
        .iter()
        .filter(|p| {
            board
                .branches
                .iter()
                .filter(|b| b.as_str() != target)
                .any(|b| {
                    p.name == board.image_package(b)
                        || p.name == board.dtb_package(b)
                        || p.name == board.uboot_package(b)
                        || p.name == board.headers_package(b)
                })
        })
        .cloned()
        .collect();
    let old_kernel: Vec<String> = old
        .iter()
        .filter(|p| !p.name.starts_with("linux-u-boot-"))
        .map(|p| p.name.clone())
        .collect();

    let env_text = root.read(ENV_FILE)?;
    let env = BootEnv::parse(&env_text);
    let mut new_env = env.clone();
    let prefixes = board.overlay_prefix_for(target, &env.list("overlay_prefix"));
    new_env.set_list("overlay_prefix", &prefixes);

    let from = st.current.clone().unwrap_or_else(|| "unknown".into());
    let mut p = Plan::new(format!(
        "Switch {} from kernel branch {from} to {target}",
        board.board_name
    ));
    p.step(
        format!("Back up {ENV_FILE}"),
        Action::Backup {
            path: ENV_FILE.into(),
        },
    );
    p.run("Refresh the package lists", &["apt-get", "update"]);
    let mut install = vec!["apt-get", "install", "-y"];
    install.extend(specs.iter().map(String::as_str));
    p.run(
        format!("Install the {target} kernel, device tree and U-Boot packages"),
        &install,
    );
    let suffix = format!("-{target}-{}", board.family_for(target));
    p.step(
        "Check that the new kernel and its device tree are in place",
        Action::VerifyKernel {
            suffix: suffix.clone(),
        },
    );
    if new_env != env {
        p.step(
            format!("Set overlay_prefix for the {target} device tree package"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: new_env.render(),
            },
        );
    }
    if !old_kernel.is_empty() && !keep_old {
        let mut rm = vec!["apt-get", "remove", "-y"];
        rm.extend(old_kernel.iter().map(String::as_str));
        p.run("Remove the old kernel and device tree packages", &rm);
    }
    let mut history = read_history(root);
    history.push(HistoryEntry {
        time: plan::utc_stamp(),
        from_branch: st.current.clone(),
        from_packages: old.clone(),
        to_branch: target.to_string(),
    });
    p.step(
        format!("Record the old branch in {HISTORY_FILE} for 'hwtool kernel revert'"),
        Action::WriteFile {
            path: HISTORY_FILE.into(),
            content: serde_json::to_string_pretty(&history)? + "\n",
        },
    );
    if !old.is_empty() {
        let mut back = vec![
            "apt-get".to_string(),
            "install".into(),
            "-y".into(),
            "--reinstall".into(),
        ];
        back.extend(old.iter().map(|p| format!("{}={}", p.name, p.version)));
        p.cleanup.push(plan::Step {
            description: "Reinstall the old packages so /boot points at the old kernel again"
                .into(),
            action: Action::Run { argv: back },
        });
    }
    let overlays = env.list("overlays");
    if !overlays.is_empty() {
        p.warnings.push(format!(
            "overlays={} was set for the {from} device tree. Overlay names differ between branches; \
             check them with 'hwtool overlays list' after the switch",
            overlays.join(" ")
        ));
    }
    p.notices.push(format!(
        "The U-Boot on the boot device is not rewritten. To write the new {} run 'hwtool boot write-bootloader'.",
        board.uboot_package(target)
    ));
    p.notices.push(format!(
        "To go back: 'hwtool kernel revert' (or 'hwtool kernel switch {from}')."
    ));
    p.reboot_required = true;
    Ok(p)
}

/// Plan the way back to the branch before the last switch.
pub fn revert_plan(root: &Root, exec: &mut dyn Executor, keep_old: bool) -> Result<Plan> {
    let history = read_history(root);
    let last = history.last().context(
        "hwtool has not switched the kernel branch on this system; there is nothing to revert",
    )?;
    let from = last
        .from_branch
        .clone()
        .context("the branch before the last switch is not known")?;
    let st = status(root)?;
    if st.current.as_deref() != Some(last.to_branch.as_str()) {
        bail!(
            "the last switch went from {from} to {}, but this system now boots {}; use 'hwtool kernel switch' instead",
            last.to_branch,
            st.current.unwrap_or_else(|| "an unknown branch".into())
        );
    }
    let want: BTreeMap<String, String> = last
        .from_packages
        .iter()
        .map(|p| (p.name.clone(), p.version.clone()))
        .collect();
    let mut p = switch_plan(root, exec, &from, keep_old, &want)?;
    p.title = format!("Revert to kernel branch {from}");
    Ok(p)
}

/// The device tree file U-Boot loads: fdtfile= in armbianEnv.txt, or the
/// U-Boot build's CONFIG_DEFAULT_FDT_FILE when the env does not set one.
pub fn fdtfile(root: &Root) -> Option<String> {
    if let Some(f) = root
        .read_opt(ENV_FILE)
        .and_then(|t| BootEnv::parse(&t).get("fdtfile"))
    {
        return Some(f);
    }
    for d in root.list_dir("/usr/lib") {
        if !d.starts_with("linux-u-boot-") {
            continue;
        }
        for f in root.list_dir(&format!("/usr/lib/{d}")) {
            if f.starts_with("u-boot-config-target-") {
                let text = root
                    .read_opt(&format!("/usr/lib/{d}/{f}"))
                    .unwrap_or_default();
                for l in text.lines() {
                    if let Some(v) = l.strip_prefix("CONFIG_DEFAULT_FDT_FILE=") {
                        return Some(v.trim_matches('"').to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn verify_installed(root: &Root, suffix: &str) -> Result<()> {
    let k = next_kernel(root).context("/boot/Image is missing or not a link")?;
    if !k.ends_with(suffix) {
        bail!("/boot/Image points at {k}, not at a kernel ending in {suffix}");
    }
    if !root.exists(&format!("/boot/vmlinuz-{k}")) {
        bail!("/boot/Image points at vmlinuz-{k}, which does not exist");
    }
    let dtb = root.read_link("/boot/dtb").unwrap_or_default();
    if !dtb.ends_with(suffix) {
        bail!("/boot/dtb points at {dtb:?}, not at the new device tree package");
    }
    match fdtfile(root) {
        Some(f) if root.exists(&format!("/boot/dtb/{f}")) => Ok(()),
        Some(f) => {
            bail!("U-Boot loads /boot/dtb/{f}, which the new device tree package does not contain")
        }
        None => Ok(()),
    }
}
