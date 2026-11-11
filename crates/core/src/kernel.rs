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
//! The switch first saves the packages it is about to replace as .deb files
//! in /var/lib/hwtool/packages (repack.rs), installs the new branch, checks
//! that `/boot/Image` and the device tree are in place, then removes the old
//! kernel packages and records the old versions and saved files so
//! `kernel revert` can go back even when the repository no longer offers
//! the old packages.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::dpkg::{self, Package};
use crate::env::BootEnv;
use crate::exec::{argv, Executor};
use crate::plan::{self, Action, Plan};
use crate::repack::{self, SavedPackage, PACKAGES_DIR};
use crate::root::Root;
use crate::ENV_FILE;

pub const HISTORY_FILE: &str = "/var/lib/hwtool/kernel-history.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub time: String,
    pub from_branch: Option<String>,
    pub from_packages: Vec<Package>,
    pub to_branch: String,
    /// The replaced packages, saved as .deb files.
    #[serde(default)]
    pub saved: Vec<SavedPackage>,
    /// `pending` from before the install until the switch has finished,
    /// then `done`. A pending entry means the switch failed or was cut off.
    #[serde(default = "done")]
    pub state: String,
}

fn done() -> String {
    "done".into()
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
    /// The board's kernel, device tree, header, U-Boot and firmware
    /// packages that are installed.
    pub hardware_packages: Vec<String>,
    /// Those of them held with apt-mark (not upgraded by apt).
    pub held: Vec<String>,
    /// Kernel modules DKMS builds (directories in /var/lib/dkms).
    pub dkms: Vec<String>,
}

pub fn read_history(root: &Root) -> Vec<HistoryEntry> {
    root.read_opt(HISTORY_FILE)
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The board's installed hardware packages, as Roxora's apt pinning names
/// them: kernel, device tree, headers and U-Boot of any branch, firmware.
pub fn hardware_packages(board: &Board, installed: &[Package]) -> Vec<String> {
    installed
        .iter()
        .filter(|p| {
            board.branches.iter().any(|b| {
                p.name == board.image_package(b)
                    || p.name == board.dtb_package(b)
                    || p.name == board.headers_package(b)
                    || p.name == board.uboot_package(b)
            }) || p.name.starts_with("armbian-firmware")
        })
        .map(|p| p.name.clone())
        .collect()
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
    let hardware_packages = hardware_packages(&board, &installed);
    let held: Vec<String> = dpkg::held(root)
        .into_iter()
        .filter(|h| hardware_packages.contains(h))
        .collect();
    Ok(Status {
        hardware_packages,
        held,
        dkms: root
            .list_dir("/var/lib/dkms")
            .into_iter()
            .filter(|d| root.path(&format!("/var/lib/dkms/{d}")).is_dir())
            .collect(),
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

/// Plan a switch to `target`. For a revert, `back` is the history entry of
/// the switch being undone: its package versions are installed from apt if
/// apt still offers them, otherwise from the .deb files saved then.
pub fn switch_plan(
    root: &Root,
    exec: &mut dyn Executor,
    target: &str,
    keep_old: bool,
    back: Option<&HistoryEntry>,
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
    if !st.held.is_empty() {
        bail!(
            "the kernel packages are held ({}); release them first with 'hwtool kernel unhold', \
             then switch",
            st.held.join(", ")
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
    let mut from_saved = Vec::new();
    for pkg in &new {
        let p = policy(exec, pkg)?;
        let want = back.and_then(|h| h.from_packages.iter().find(|x| &x.name == pkg));
        let mut saved = back
            .and_then(|h| h.saved.iter().find(|x| &x.name == pkg))
            .filter(|x| root.exists(&x.file));
        // A saved file is used only if dpkg-deb can read it.
        if let Some(f) = saved {
            if !repack::is_valid(root, exec, &f.file, &f.name) {
                crate::out::say(&format!("{} is damaged; not using it", f.file));
                saved = None;
            }
        }
        match (want, saved, &p.candidate) {
            (Some(w), _, _) if p.versions.contains(&w.version) => {
                specs.push(format!("{pkg}={}", w.version))
            }
            (_, Some(f), _) => {
                specs.push(f.file.clone());
                from_saved.push(format!("{pkg} {} from {}", f.version, f.file));
            }
            (_, None, Some(_)) => specs.push(pkg.clone()),
            (_, None, None) => missing.push(pkg.clone()),
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
    let time = plan::utc_stamp();
    let mut save_dir = format!("{PACKAGES_DIR}/{time}");
    let mut n = 1;
    while root.exists(&save_dir) {
        save_dir = format!("{PACKAGES_DIR}/{time}-{n}");
        n += 1;
    }
    // apt replaces the old U-Boot package; the kernel packages go too unless kept.
    let to_save: Vec<&Package> = old
        .iter()
        .filter(|p| p.name.starts_with("linux-u-boot-") || !keep_old)
        .collect();
    let mut saved = Vec::new();
    let mut save_steps = Vec::new();
    let mut need_kib = 0;
    for pkg in &to_save {
        let sp = repack::plan_save(root, &pkg.name, &pkg.version, &save_dir)?;
        need_kib += sp.installed_kib;
        saved.push(SavedPackage {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            file: sp.file.clone(),
        });
        save_steps.push(plan::Step::new(
            format!(
                "Save {} {} for 'hwtool kernel revert'",
                pkg.name, pkg.version
            ),
            Action::SavePackage {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                dir: save_dir.clone(),
                file: sp.file,
                from_cache: sp.from_cache,
                installed_kib: sp.installed_kib,
            },
        ));
    }
    repack::check_space(root, &save_dir, need_kib)?;

    // What apt itself would do, so the plan shows apt's resolution and
    // stops on removals nobody asked for.
    let mut install_sim = argv(&["apt-get", "-s", "-y", "install"]);
    install_sim.extend(specs.iter().cloned());
    let sim = apt_simulate(exec, &install_sim)?;
    let unexpected: Vec<&String> = sim
        .removed
        .iter()
        .filter(|r| !(r.starts_with("linux-u-boot-") && old.iter().any(|o| &&o.name == r)))
        .collect();
    if !unexpected.is_empty() {
        bail!(
            "installing the {target} packages would also remove {}; hwtool stops here. \
             Check with: {}",
            unexpected
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            plan::shell_join(&install_sim)
        );
    }
    let remove_sim = if !old_kernel.is_empty() && !keep_old {
        let mut a = argv(&["apt-get", "-s", "-y", "remove"]);
        a.extend(old_kernel.iter().cloned());
        let r = apt_simulate(exec, &a)?;
        let extra: Vec<&String> = r
            .removed
            .iter()
            .filter(|x| !old_kernel.contains(x))
            .collect();
        if !extra.is_empty() {
            bail!(
                "removing the old kernel packages would also remove {}; hwtool stops here. \
                 Use --keep-old, or check with: {}",
                extra
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                plan::shell_join(&a)
            );
        }
        Some(r)
    } else {
        None
    };

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
    p.steps.extend(save_steps);
    p.run("Refresh the package lists", &["apt-get", "update"]);

    let mut history = read_history(root);
    let entry = HistoryEntry {
        time,
        from_branch: st.current.clone(),
        from_packages: old.clone(),
        to_branch: target.to_string(),
        saved: saved.clone(),
        state: "pending".into(),
    };
    history.push(entry.clone());
    p.step(
        format!("Record the switch as pending in {HISTORY_FILE}"),
        Action::WriteFile {
            path: HISTORY_FILE.into(),
            content: serde_json::to_string_pretty(&history)? + "\n",
        },
    );

    let install_at = p.steps.len();
    let mut install = vec!["apt-get", "install", "-y"];
    install.extend(specs.iter().map(String::as_str));
    p.run(
        format!("Install the {target} kernel, device tree and U-Boot packages"),
        &install,
    );
    p.steps[install_at].details = sim.lines;
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
    if let Some(r) = remove_sim {
        let mut rm = vec!["apt-get", "remove", "-y"];
        rm.extend(old_kernel.iter().map(String::as_str));
        p.run("Remove the old kernel and device tree packages", &rm);
        let last = p.steps.len() - 1;
        p.steps[last].details = r.lines;
    }
    let rollback_to = p.steps.len() - 1;
    if let Some(last) = history.last_mut() {
        last.state = "done".into();
    }
    p.step(
        format!("Record the finished switch in {HISTORY_FILE} for 'hwtool kernel revert'"),
        Action::WriteFile {
            path: HISTORY_FILE.into(),
            content: serde_json::to_string_pretty(&history)? + "\n",
        },
    );
    if !saved.is_empty() {
        p.step(
            "Remove the packages saved by earlier switches",
            Action::PruneSaved {
                keep: save_dir.clone(),
            },
        );
    }

    // Rollback, only once the install has started: put armbianEnv.txt back,
    // purge what the switch installed, reinstall what it replaced, and show
    // where /boot points afterwards.
    p.on_failure(
        install_at,
        rollback_to,
        format!("Put back {ENV_FILE} from this run's backup"),
        Action::RestoreBackup {
            path: ENV_FILE.into(),
        },
    );
    let fresh: Vec<String> = new
        .iter()
        .filter(|n| !installed.iter().any(|p| &&p.name == n))
        .cloned()
        .collect();
    if !fresh.is_empty() && !old.is_empty() {
        let mut purge = argv(&["apt-get", "purge", "-y"]);
        purge.extend(fresh.iter().cloned());
        p.on_failure(
            install_at,
            rollback_to,
            format!("Purge the {target} packages this switch installed"),
            Action::Run { argv: purge },
        );
    }
    if !old.is_empty() {
        // Use a saved file only if the save step got that far.
        let mut script = String::from("set -e\npkgs=()\n");
        let mut shows = Vec::new();
        for o in &old {
            let spec = format!("{}={}", o.name, o.version);
            match saved.iter().find(|s| s.name == o.name) {
                Some(f) => {
                    script.push_str(&format!(
                        "if [ -f '{0}' ]; then pkgs+=('{0}'); else pkgs+=('{spec}'); fi\n",
                        f.file
                    ));
                    shows.push(format!("{} (or {spec} if it was not saved)", f.file));
                }
                None => {
                    script.push_str(&format!("pkgs+=('{spec}')\n"));
                    shows.push(spec);
                }
            }
        }
        script.push_str("apt-get install -y --reinstall --allow-downgrades \"${pkgs[@]}\"\n");
        p.on_failure(
            install_at,
            rollback_to,
            "Reinstall the old packages so /boot points at the old kernel again",
            Action::Shell {
                script,
                shows: vec![format!(
                    "apt-get install -y --reinstall --allow-downgrades {}",
                    shows.join(" ")
                )],
            },
        );
    }
    p.on_failure(
        install_at,
        rollback_to,
        "Show where /boot points now",
        Action::ReportBoot,
    );

    let overlays = env.list("overlays");
    if !overlays.is_empty() {
        p.warnings.push(format!(
            "overlays={} was set for the {from} device tree. Overlay names differ between branches; \
             check them with 'hwtool overlays list' after the switch",
            overlays.join(" ")
        ));
    }
    if target.starts_with("vendor") {
        if let Some(c) = installed.iter().find(|p| p.name == "charsiu") {
            p.notices.push(format!(
                "charsiu {} is installed. It needs the edge kernel's rocket driver and will not \
                 work on the {target} branch until you go back ('hwtool kernel revert').",
                c.version
            ));
        }
    }
    p.notices.push(format!(
        "The U-Boot on the boot device is not rewritten. To write the new {} run 'hwtool boot write-bootloader'.",
        board.uboot_package(target)
    ));
    for f in &from_saved {
        p.notices.push(format!(
            "apt no longer offers this version; installing {f} (saved by the last switch)."
        ));
    }
    p.notices.push(format!(
        "To go back: 'hwtool kernel revert' (or 'hwtool kernel switch {from}')."
    ));
    p.reboot_required = true;
    Ok(p)
}

struct AptSim {
    /// Inst, Remv and Purg lines, as apt printed them.
    lines: Vec<String>,
    removed: Vec<String>,
}

fn apt_simulate(exec: &mut dyn Executor, args: &[String]) -> Result<AptSim> {
    let out = exec
        .capture(args)
        .with_context(|| format!("apt cannot resolve: {}", plan::shell_join(args)))?;
    let mut lines = vec![format!("apt resolves ({}):", plan::shell_join(args))];
    let mut removed = Vec::new();
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("Remv ").or_else(|| l.strip_prefix("Purg ")) {
            removed.push(rest.split_whitespace().next().unwrap_or("").to_string());
            lines.push(format!("  {l}"));
        } else if l.starts_with("Inst ") {
            lines.push(format!("  {l}"));
        }
    }
    Ok(AptSim { lines, removed })
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
    if last.state == "pending" {
        if st.current.as_deref() == Some(from.as_str()) {
            bail!(
                "the last switch, from {from} to {}, did not finish and this system boots {from} \
                 again; there is nothing to revert. 'hwtool kernel status' shows the packages",
                last.to_branch
            );
        }
        crate::out::say(&format!(
            "The last switch, from {from} to {}, did not finish; going back to {from}.",
            last.to_branch
        ));
    } else if st.current.as_deref() != Some(last.to_branch.as_str()) {
        bail!(
            "the last switch went from {from} to {}, but this system now boots {}; use 'hwtool kernel switch' instead",
            last.to_branch,
            st.current.unwrap_or_else(|| "an unknown branch".into())
        );
    }
    let mut p = switch_plan(root, exec, &from, keep_old, Some(last))?;
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

/// Hold or release the board's hardware packages with apt-mark, so apt
/// upgrades do not change the kernel, device tree, U-Boot or firmware.
pub fn hold_plan(root: &Root, hold: bool) -> Result<Plan> {
    let st = status(root)?;
    let targets: Vec<String> = if hold {
        st.hardware_packages
            .iter()
            .filter(|p| !st.held.contains(p))
            .cloned()
            .collect()
    } else {
        st.held.clone()
    };
    let mut p = Plan::new(if hold {
        "Hold the kernel, device tree, U-Boot and firmware packages".to_string()
    } else {
        "Release the held kernel, device tree, U-Boot and firmware packages".to_string()
    });
    if targets.is_empty() {
        p.notices.push(if hold {
            "They are already held.".into()
        } else {
            "None of them is held.".into()
        });
        return Ok(p);
    }
    let mut a = vec!["apt-mark", if hold { "hold" } else { "unhold" }];
    a.extend(targets.iter().map(String::as_str));
    p.run(
        if hold {
            "Hold the packages (apt will not upgrade or remove them)"
        } else {
            "Release the packages (apt upgrades them again)"
        },
        &a,
    );
    if hold {
        p.notices.push(
            "Security fixes for the kernel arrive through these packages; while they are held \
             they do not reach this system. 'hwtool kernel switch' refuses until you release them."
                .into(),
        );
    }
    Ok(p)
}

/// Install or remove the headers of the branch in use
/// (linux-headers-<branch>-<family>; Roxora's pinning allows them).
pub fn headers_plan(root: &Root, exec: &mut dyn Executor, install: bool) -> Result<Plan> {
    let st = status(root)?;
    let branch = st
        .current
        .clone()
        .context("cannot tell which kernel branch is in use")?;
    let pkg = st.board.headers_package(&branch);
    let installed = dpkg::installed(root);
    let have = installed.iter().find(|p| p.name == pkg);
    if install && have.is_some() {
        bail!("{pkg} is already installed");
    }
    if !install && have.is_none() {
        bail!("{pkg} is not installed");
    }
    if st.held.contains(&pkg) {
        bail!("{pkg} is held; release it first with 'hwtool kernel unhold'");
    }
    if install && policy(exec, &pkg)?.candidate.is_none() {
        bail!("apt has no installable version of {pkg}; check the Armbian repository and run apt-get update");
    }
    let verb = if install { "install" } else { "remove" };
    let sim = argv(&["apt-get", "-s", "-y", "--no-install-recommends", verb, &pkg]);
    let r = apt_simulate(exec, &sim)?;
    if !r.removed.iter().all(|x| *x == pkg) {
        let extra: Vec<&String> = r.removed.iter().filter(|x| **x != pkg).collect();
        bail!(
            "{verb}ing {pkg} would also remove {}; hwtool stops here. Check with: {}",
            extra
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            plan::shell_join(&sim)
        );
    }
    let mut p = Plan::new(format!(
        "{} the kernel headers for {} ({pkg})",
        if install { "Install" } else { "Remove" },
        st.kernel.as_deref().unwrap_or(&branch)
    ));
    p.run(
        format!("{} {pkg}", if install { "Install" } else { "Remove" }),
        &["apt-get", verb, "-y", "--no-install-recommends", &pkg],
    );
    p.steps[0].details = r.lines;
    if install {
        p.notices.push(
            "With headers installed, DKMS builds its modules for each new kernel of this branch."
                .into(),
        );
    } else if !st.dkms.is_empty() {
        p.warnings.push(format!(
            "DKMS modules are installed ({}). Without headers they cannot be rebuilt when the \
             kernel is next updated, and will be missing from the new kernel{}.",
            st.dkms.join(", "),
            if st.dkms.iter().any(|d| d.contains("aic8800")) {
                " (aic8800 is the ROCK 4D's Wi-Fi and Bluetooth driver)"
            } else {
                ""
            }
        ));
    }
    Ok(p)
}
