//! The NPU and charsiu.
//!
//! charsiu (https://github.com/gahingwoo/charsiu) runs language models on the
//! RK3576 NPU through the mainline `rocket` DRM accel driver: the edge kernel
//! ships it as drivers/accel/rocket/rocket.ko, the device tree enables the
//! NPU cores (compatible "rockchip,rk3576-rknn-core"), and the driver gives
//! /dev/accel/accel0. Vendor kernels have Rockchip's own `rknpu` driver
//! instead, which charsiu does not use. charsiu itself is a package in the
//! Armbian repository (noble-utils) that Roxora's apt pinning allows.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use anyhow::{bail, Result};
use serde::Serialize;

use crate::board::Board;
use crate::dpkg;
use crate::exec::{argv, Executor};
use crate::plan::{self, Action, Plan};
use crate::root::Root;

/// Device tree compatibles of NPU cores driven by `rocket` (mainline).
pub const ROCKET_COMPATIBLES: &[&str] = &["rockchip,rk3576-rknn-core", "rockchip,rk3588-rknn-core"];
/// Device tree compatibles of the vendor `rknpu` driver.
pub const RKNPU_COMPATIBLES: &[&str] = &[
    "rockchip,rk3576-rknpu",
    "rockchip,rk3588-rknpu",
    "rockchip,rknpu",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Driver {
    pub name: String,
    /// The module file for the kernel looked at, if there is one.
    pub module_file: Option<String>,
    /// Built into the kernel (modules.builtin).
    pub builtin: bool,
    /// Loaded in the running kernel (/proc/modules or /sys/module).
    pub loaded: bool,
}

impl Driver {
    pub fn available(&self) -> bool {
        self.module_file.is_some() || self.builtin
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DtNode {
    pub path: String,
    pub compatible: Vec<String>,
    /// `okay` when the node has no status property.
    pub status: String,
}

impl DtNode {
    pub fn enabled(&self) -> bool {
        self.status == "okay" || self.status == "ok"
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AccelDevice {
    pub path: String,
    /// For example `crw-rw----`.
    pub mode: String,
    pub owner: String,
    pub group: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Charsiu {
    pub installed: Option<String>,
    pub candidate: Option<String>,
    /// Where the candidate comes from, as apt-cache policy shows it.
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    /// The kernel the facts below are about.
    pub kernel: Option<String>,
    pub branch: Option<String>,
    /// The running kernel, when it is not `kernel` (for example right after
    /// a branch switch, before the reboot).
    pub running_kernel: Option<String>,
    pub rocket: Driver,
    pub rknpu: Driver,
    pub accel: Vec<AccelDevice>,
    pub nodes: Vec<DtNode>,
    pub charsiu: Charsiu,
    /// The NPU can run charsiu now.
    pub ready: bool,
    /// charsiu can be installed: edge-type kernel with rocket and an
    /// enabled NPU node. It may still need a reboot or `modprobe rocket`.
    pub can_install: bool,
    pub verdict: String,
}

fn branch_of(board: &Board, kver: &str) -> Option<String> {
    board
        .branches
        .iter()
        .find(|b| kver.ends_with(&format!("-{b}-{}", board.family_for(b))))
        .cloned()
}

fn module(root: &Root, kver: Option<&str>, running: bool, name: &str, rel: &[&str]) -> Driver {
    let mut d = Driver {
        name: name.into(),
        module_file: None,
        builtin: false,
        loaded: false,
    };
    if let Some(k) = kver {
        for r in rel {
            for ext in ["", ".zst", ".xz", ".gz"] {
                let f = format!("/lib/modules/{k}/kernel/{r}{ext}");
                if root.exists(&f) || root.exists(&format!("/usr{f}")) {
                    d.module_file = Some(f);
                }
            }
        }
        let builtin = root
            .read_opt(&format!("/lib/modules/{k}/modules.builtin"))
            .unwrap_or_default();
        d.builtin = rel.iter().any(|r| builtin.lines().any(|l| l == *r));
    }
    if running {
        let mods = root.read_opt("/proc/modules").unwrap_or_default();
        d.loaded = mods
            .lines()
            .any(|l| l.split_whitespace().next() == Some(name))
            || root.exists(&format!("/sys/module/{name}"));
    }
    d
}

fn dt_base(root: &Root) -> Option<&'static str> {
    ["/sys/firmware/devicetree/base", "/proc/device-tree"]
        .into_iter()
        .find(|b| root.exists(b))
}

fn read_strings(root: &Root, path: &str) -> Vec<String> {
    fs::read(root.path(path))
        .map(|b| {
            b.split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// NPU nodes: the root's children and their children (NPU cores sit under
/// /soc on RK3576 and at the top level on RK3588).
pub fn dt_nodes(root: &Root) -> Vec<DtNode> {
    let Some(base) = dt_base(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut dirs = vec![base.to_string()];
    for top in root.list_dir(base) {
        let p = format!("{base}/{top}");
        if root.path(&p).is_dir() {
            dirs.push(p.clone());
            for sub in root.list_dir(&p) {
                let q = format!("{p}/{sub}");
                if root.path(&q).is_dir() {
                    dirs.push(q);
                }
            }
        }
    }
    for d in dirs {
        let compatible = read_strings(root, &format!("{d}/compatible"));
        if compatible.iter().any(|c| {
            ROCKET_COMPATIBLES.contains(&c.as_str()) || RKNPU_COMPATIBLES.contains(&c.as_str())
        }) {
            let status = read_strings(root, &format!("{d}/status"))
                .into_iter()
                .next()
                .unwrap_or_else(|| "okay".into());
            out.push(DtNode {
                path: d.strip_prefix(base).unwrap_or(&d).to_string(),
                compatible,
                status,
            });
        }
    }
    out
}

fn name_of(root: &Root, file: &str, id: u32) -> String {
    root.read_opt(file)
        .unwrap_or_default()
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 2 && f[2] == id.to_string()).then(|| f[0].to_string())
        })
        .unwrap_or_else(|| id.to_string())
}

fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o020000 => 'c',
        0o060000 => 'b',
        0o040000 => 'd',
        _ => '-',
    };
    let mut s = String::from(kind);
    for shift in [6, 3, 0] {
        let m = (mode >> shift) & 7;
        s.push(if m & 4 != 0 { 'r' } else { '-' });
        s.push(if m & 2 != 0 { 'w' } else { '-' });
        s.push(if m & 1 != 0 { 'x' } else { '-' });
    }
    s
}

pub fn accel_devices(root: &Root) -> Vec<AccelDevice> {
    root.list_dir("/dev/accel")
        .into_iter()
        .filter(|n| n.starts_with("accel"))
        .filter_map(|n| {
            let path = format!("/dev/accel/{n}");
            let m = fs::metadata(root.path(&path)).ok()?;
            Some(AccelDevice {
                mode: mode_string(m.permissions().mode()),
                owner: name_of(root, "/etc/passwd", m.uid()),
                group: name_of(root, "/etc/group", m.gid()),
                path,
            })
        })
        .collect()
}

pub fn charsiu(root: &Root, exec: &mut dyn Executor) -> Charsiu {
    let installed = dpkg::installed(root)
        .into_iter()
        .find(|p| p.name == "charsiu")
        .map(|p| p.version);
    let policy = exec
        .capture(&argv(&["apt-cache", "policy", "charsiu"]))
        .unwrap_or_default();
    let p = dpkg::parse_policy(&policy);
    // The source line under the candidate in the version table.
    let origin = p.candidate.as_ref().and_then(|c| {
        let mut after = false;
        for l in policy.lines() {
            let t = l.trim().trim_start_matches("***").trim();
            if t.starts_with(&format!("{c} ")) {
                after = true;
                continue;
            }
            if after {
                let f: Vec<&str> = t.split_whitespace().collect();
                if f.len() >= 2 && f[0].parse::<i32>().is_ok() {
                    return Some(f[1..].join(" "));
                }
                break;
            }
        }
        None
    });
    Charsiu {
        installed,
        candidate: p.candidate,
        origin,
    }
}

pub fn status(root: &Root, exec: &mut dyn Executor) -> Result<Status> {
    let board = Board::detect(root)?;
    let running = root
        .read_opt("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let next = root
        .read_link("/boot/Image")
        .and_then(|t| t.rsplit('/').next().map(str::to_string))
        .map(|n| n.strip_prefix("vmlinuz-").unwrap_or(&n).to_string());
    // Look at the running kernel when it is one of this board's; otherwise
    // (a container, a chroot) at the kernel the board boots next.
    let running_ours = running
        .as_deref()
        .is_some_and(|k| branch_of(&board, k).is_some());
    let kernel = if running_ours {
        running.clone()
    } else {
        next.clone()
    };
    let branch = kernel.as_deref().and_then(|k| branch_of(&board, k));
    let rocket = module(
        root,
        kernel.as_deref(),
        running_ours,
        "rocket",
        &["drivers/accel/rocket/rocket.ko"],
    );
    let rknpu = module(
        root,
        kernel.as_deref(),
        running_ours,
        "rknpu",
        &["drivers/rknpu/rknpu.ko"],
    );
    let accel = accel_devices(root);
    let nodes = dt_nodes(root);
    let charsiu = charsiu(root, exec);
    let rocket_nodes: Vec<&DtNode> = nodes
        .iter()
        .filter(|n| {
            n.compatible
                .iter()
                .any(|c| ROCKET_COMPATIBLES.contains(&c.as_str()))
        })
        .collect();
    let vendor = branch.as_deref().is_some_and(|b| b.starts_with("vendor"))
        || (!rocket.available() && (rknpu.available() || rknpu.loaded));
    let node_on = rocket_nodes.iter().any(|n| n.enabled());
    let can_install = !vendor && rocket.available() && node_on;
    let ready = can_install && rocket.loaded && !accel.is_empty();
    let verdict = if vendor {
        "vendor kernel: charsiu needs the edge kernel's rocket driver (hwtool kernel switch edge)"
            .to_string()
    } else if !rocket.available() {
        "the kernel has no NPU driver (no rocket module)".to_string()
    } else if rocket_nodes.is_empty() {
        "the device tree has no NPU node for the rocket driver".to_string()
    } else if !node_on {
        "the NPU is disabled in the device tree".to_string()
    } else if !running_ours {
        format!(
            "the rocket driver is there for {}; reboot into it to use the NPU",
            kernel.as_deref().unwrap_or("the next kernel")
        )
    } else if !rocket.loaded {
        "the rocket driver is not loaded (sudo modprobe rocket)".to_string()
    } else if accel.is_empty() {
        "rocket is loaded but there is no /dev/accel device".to_string()
    } else if let Some(v) = &charsiu.installed {
        format!("NPU ready; charsiu {v} is installed")
    } else {
        "NPU ready for charsiu (sudo hwtool npu install-charsiu)".to_string()
    };
    Ok(Status {
        running_kernel: running.filter(|r| Some(r) != kernel.as_ref()),
        kernel,
        branch,
        rocket,
        rknpu,
        accel,
        nodes,
        charsiu,
        ready,
        can_install,
        verdict,
    })
}

/// (Inst|Remv|Purg, package) pairs from a simulated apt run.
type AptOps = Vec<(String, String)>;

/// Lines apt prints for a simulated run, and its operations.
fn simulate(exec: &mut dyn Executor, args: &[String]) -> Result<(Vec<String>, AptOps)> {
    let out = exec.capture(args)?;
    let mut lines = vec![format!("apt resolves ({}):", plan::shell_join(args))];
    let mut ops = Vec::new();
    for l in out.lines() {
        for op in ["Inst", "Remv", "Purg"] {
            if let Some(rest) = l.strip_prefix(&format!("{op} ")) {
                ops.push((
                    op.to_string(),
                    rest.split_whitespace().next().unwrap_or("").to_string(),
                ));
                lines.push(format!("  {l}"));
            }
        }
    }
    Ok((lines, ops))
}

pub fn install_plan(root: &Root, exec: &mut dyn Executor) -> Result<Plan> {
    let st = status(root, exec)?;
    if !st.can_install {
        bail!("not installing charsiu: {}", st.verdict);
    }
    if let Some(v) = &st.charsiu.installed {
        bail!("charsiu {v} is already installed");
    }
    if st.charsiu.candidate.is_none() {
        bail!(
            "apt has no charsiu package. It comes from the Armbian repository (noble-utils); \
             check that the repository is enabled and run apt-get update"
        );
    }
    // Only charsiu: its Recommends (curl or wget, whiptail) are named in a
    // notice instead of being pulled in.
    let sim = argv(&[
        "apt-get",
        "-s",
        "-y",
        "--no-install-recommends",
        "install",
        "charsiu",
    ]);
    let (lines, ops) = simulate(exec, &sim)?;
    let other: Vec<String> = ops
        .iter()
        .filter(|(op, p)| !(op == "Inst" && p == "charsiu"))
        .map(|(op, p)| format!("{op} {p}"))
        .collect();
    if !other.is_empty() {
        bail!(
            "installing charsiu would also do: {}; hwtool installs charsiu only. Check with: {}",
            other.join(", "),
            plan::shell_join(&sim)
        );
    }
    if !ops.iter().any(|(op, p)| op == "Inst" && p == "charsiu") {
        bail!("apt would not install charsiu: {}", lines.join(" "));
    }
    let mut p = Plan::new(format!(
        "Install charsiu {} from {}",
        st.charsiu.candidate.as_deref().unwrap_or(""),
        st.charsiu.origin.as_deref().unwrap_or("apt")
    ));
    p.run(
        "Install charsiu",
        &[
            "apt-get",
            "install",
            "-y",
            "--no-install-recommends",
            "charsiu",
        ],
    );
    p.steps[0].details = lines;
    p.step("Show the NPU status", Action::NpuStatus);
    p.notices.push(
        "charsiu does not come with a model. Fetch one with 'charsiu pull' (no name opens a picker)."
            .into(),
    );
    p.notices.push(
        "charsiu recommends curl or wget (to fetch models) and whiptail (for its menus); hwtool \
         installs charsiu alone. Install them with apt if they are missing."
            .into(),
    );
    if !st.ready {
        p.notices
            .push(format!("Before charsiu can use the NPU: {}.", st.verdict));
    }
    Ok(p)
}

pub fn remove_plan(root: &Root, exec: &mut dyn Executor) -> Result<Plan> {
    let st = status(root, exec)?;
    let Some(v) = &st.charsiu.installed else {
        bail!("charsiu is not installed");
    };
    let sim = argv(&["apt-get", "-s", "-y", "remove", "charsiu"]);
    let (lines, ops) = simulate(exec, &sim)?;
    let other: Vec<String> = ops
        .iter()
        .filter(|(op, p)| !(op == "Remv" && p == "charsiu"))
        .map(|(op, p)| format!("{op} {p}"))
        .collect();
    if !other.is_empty() {
        bail!(
            "removing charsiu would also do: {}; hwtool removes charsiu only. Check with: {}",
            other.join(", "),
            plan::shell_join(&sim)
        );
    }
    let mut p = Plan::new(format!("Remove charsiu {v}"));
    p.run("Remove charsiu", &["apt-get", "remove", "-y", "charsiu"]);
    p.steps[0].details = lines;
    p.step("Show the NPU status", Action::NpuStatus);
    p.notices.push(
        "Models charsiu downloaded are not part of the package and stay where charsiu put \
         them (by default /opt/charsiu/models)."
            .into(),
    );
    Ok(p)
}

/// Print the status in a few lines (used after install and remove).
pub fn report(root: &Root, exec: &mut dyn Executor) {
    match status(root, exec) {
        Ok(s) => {
            crate::out::say(&format!(
                "    charsiu: {}",
                s.charsiu.installed.as_deref().unwrap_or("not installed")
            ));
            crate::out::say(&format!("    NPU: {}", s.verdict));
        }
        Err(e) => crate::out::say(&format!("    NPU status: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn modes() {
        assert_eq!(super::mode_string(0o020660), "crw-rw----");
        assert_eq!(super::mode_string(0o100644), "-rw-r--r--");
    }
}
