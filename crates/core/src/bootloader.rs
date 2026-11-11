//! Writing U-Boot with the board's own U-Boot package.
//!
//! The `linux-u-boot-<board>-<branch>` package ships the binaries in
//! `/usr/lib/linux-u-boot-<branch>-<board>/` and the shell functions that
//! write them in `/usr/lib/u-boot/platform_install.sh`
//! (lib/functions/compilation/uboot.sh in armbian/build):
//! `write_uboot_platform <dir> <disk>` for SD/eMMC and
//! `write_uboot_platform_mtd <dir> <mtdblock> <log> <mtd list> [image]` for
//! SPI flash. hwtool sources that file and calls those functions; it does not
//! know offsets or file names itself.

use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::dpkg;
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::storage::{self, Disk, Mtd};

pub const PLATFORM_INSTALL: &str = "/usr/lib/u-boot/platform_install.sh";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UbootPackage {
    /// Installed linux-u-boot-* package and version, if dpkg knows it.
    pub package: Option<String>,
    pub version: Option<String>,
    /// DIR from platform_install.sh: where the binaries are.
    pub dir: String,
    /// Writer functions platform_install.sh defines.
    pub functions: Vec<String>,
    pub files: Vec<String>,
    pub mtd_image: Option<String>,
}

impl UbootPackage {
    pub fn has(&self, f: &str) -> bool {
        self.functions.iter().any(|x| x == f)
    }
}

pub fn detect(root: &Root) -> Option<UbootPackage> {
    let text = root.read_opt(PLATFORM_INSTALL)?;
    let mut dir = None;
    let mut mtd_image = None;
    let mut functions = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("DIR=") {
            dir = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = t.strip_prefix("BOOTLOADER_MTD_IMAGE=") {
            mtd_image = Some(
                v.trim_matches('"')
                    .trim_matches('\\')
                    .trim_matches('"')
                    .to_string(),
            );
        }
        for f in [
            "write_uboot_platform",
            "write_uboot_platform_mtd",
            "write_uboot_platform_ufs",
        ] {
            if let Some(rest) = t.strip_prefix(f) {
                if rest.trim_start().starts_with("()") && !functions.iter().any(|x| x == f) {
                    functions.push(f.to_string());
                }
            }
        }
    }
    let dir = dir?;
    let pkg = dpkg::installed(root)
        .into_iter()
        .find(|p| p.name.starts_with("linux-u-boot-"));
    Some(UbootPackage {
        package: pkg.as_ref().map(|p| p.name.clone()),
        version: pkg.map(|p| p.version),
        files: root.list_dir(&dir),
        dir,
        functions,
        mtd_image,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub uboot: Option<UbootPackage>,
    pub root_device: Option<String>,
    pub disks: Vec<Disk>,
    pub spi: Vec<Mtd>,
    /// Plain-language reasons why an operation is not available.
    pub missing: Vec<String>,
}

pub fn status(root: &Root) -> Status {
    let uboot = detect(root);
    let mut missing = Vec::new();
    match &uboot {
        None => missing.push(format!(
            "{PLATFORM_INSTALL} is missing: no linux-u-boot-* package is installed, so the bootloader cannot be written"
        )),
        Some(u) => {
            if !u.has("write_uboot_platform") {
                missing.push("the U-Boot package has no write_uboot_platform function: it cannot write SD or eMMC".into());
            }
            if !u.has("write_uboot_platform_mtd") {
                missing.push("the U-Boot package has no write_uboot_platform_mtd function: this board's U-Boot is not built for SPI flash".into());
            }
        }
    }
    let spi = storage::mtds(root);
    if spi.is_empty() {
        missing.push("no SPI flash (MTD device) is visible to the kernel".into());
    }
    Status {
        uboot,
        root_device: storage::root_device(root),
        disks: storage::disks(root),
        spi,
        missing,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Spi,
    Disk(String),
}

impl Target {
    pub fn parse(s: &str) -> Result<Target> {
        match s {
            "spi" | "mtd" => Ok(Target::Spi),
            d if d.starts_with("/dev/") => Ok(Target::Disk(d.to_string())),
            _ => bail!("target must be 'spi' or a whole SD/eMMC device such as /dev/mmcblk0"),
        }
    }
}

/// The bash script that writes U-Boot with the package's own function.
fn writer_script(func_call: &str) -> String {
    format!("set -e -o pipefail\nsource {PLATFORM_INSTALL}\n{func_call} | cat\nsync\n")
}

/// Run the writer function with `dd` and `flashcp` replaced by stubs that
/// print their arguments, to show exactly what the real run would write.
/// Nothing is written.
pub fn expand(root: &Root, func_call: &str) -> Result<Vec<String>> {
    let prefix = if root.is_system() {
        String::new()
    } else {
        root.base().display().to_string()
    };
    let script = format!(
        "dd() {{ printf 'dd'; printf ' %s' \"$@\"; printf '\\n'; }}\n\
         flashcp() {{ printf 'flashcp'; printf ' %s' \"$@\"; printf '\\n'; }}\n\
         dialog() {{ return 1; }}\n\
         source \"$HWTOOL_ROOT{PLATFORM_INSTALL}\"\n\
         DIR=\"$HWTOOL_ROOT$DIR\"\n\
         {func_call} | cat\n"
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("HWTOOL_ROOT", &prefix)
        .output()
        .context("cannot run bash")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!(
            "the U-Boot package's writer refused: {}{}",
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let lines: Vec<String> = stdout
        .lines()
        .filter(|l| l.starts_with("dd ") || l.starts_with("flashcp "))
        .map(|l| {
            if prefix.is_empty() {
                l.to_string()
            } else {
                l.replace(&prefix, "")
            }
        })
        .collect();
    if lines.is_empty() {
        bail!(
            "the U-Boot package's writer does not write anything: {}",
            stdout.trim()
        );
    }
    Ok(lines)
}

/// End offset in bytes of a `dd ... of=<disk>` write, if it targets `disk`.
fn dd_end(root: &Root, cmd: &str, disk: &str) -> Option<u64> {
    let mut input = None;
    let mut output = None;
    let mut bs = 512u64;
    let mut seek = 0u64;
    for a in cmd.split_whitespace().skip(1) {
        let (k, v) = a.split_once('=')?;
        match k {
            "if" => input = Some(v.to_string()),
            "of" => output = Some(v.to_string()),
            "bs" | "obs" => bs = parse_size(v)?,
            "seek" | "oseek" => seek = v.parse().ok()?,
            _ => {}
        }
    }
    if output.as_deref() != Some(disk) {
        return None;
    }
    let len = std::fs::metadata(root.path(&input?)).ok()?.len();
    Some(seek * bs + len)
}

fn parse_size(v: &str) -> Option<u64> {
    let (num, mul) = match v.chars().last()? {
        'k' | 'K' => (&v[..v.len() - 1], 1024),
        'M' => (&v[..v.len() - 1], 1024 * 1024),
        _ => (v, 1),
    };
    num.parse::<u64>().ok().map(|n| n * mul)
}

pub fn write_plan(root: &Root, target: &Target, allow_booted: bool) -> Result<Plan> {
    write_plan_with(root, target, allow_booted, None)
}

/// Like [`write_plan`]; `planned_start` is the first partition's start in
/// bytes after a repartition that has not happened yet (migration).
pub fn write_plan_with(
    root: &Root,
    target: &Target,
    allow_booted: bool,
    planned_start: Option<u64>,
) -> Result<Plan> {
    let u = detect(root).with_context(|| {
        format!("{PLATFORM_INSTALL} is missing: install the board's linux-u-boot-<board>-<branch> package first")
    })?;
    let pkg = u
        .package
        .clone()
        .unwrap_or_else(|| "the U-Boot package".into());
    match target {
        Target::Spi => {
            if !u.has("write_uboot_platform_mtd") {
                bail!("{pkg} has no write_uboot_platform_mtd function: this board's U-Boot is not built for SPI flash");
            }
            let spi = storage::mtds(root);
            let first = spi
                .first()
                .context("no SPI flash is visible to the kernel (no /sys/class/mtd/mtd0)")?;
            let list: Vec<String> = spi
                .iter()
                .map(|m| m.name.replace("mtd", "mtdblock"))
                .collect();
            let image = u
                .mtd_image
                .as_ref()
                .map(|i| format!(" \"$DIR/{i}\""))
                .unwrap_or_default();
            let call = format!(
                "write_uboot_platform_mtd \"$DIR\" /dev/{} /dev/null \"{}\"{image}",
                first.name.replace("mtd", "mtdblock"),
                list.join(" ")
            );
            let shows = expand(root, &call)?;
            if shows.iter().any(|s| s.starts_with("flashcp")) && !root.has_program("flashcp") {
                bail!("flashcp is not installed; install the mtd-utils package");
            }
            let mut plan = Plan::new(format!(
                "Write U-Boot from {pkg} to SPI flash {}",
                first.path
            ));
            plan.step(
                format!(
                    "Write U-Boot to SPI flash ({} {} bytes) with {pkg}'s write_uboot_platform_mtd",
                    first.label, first.size_bytes
                ),
                Action::Shell {
                    script: writer_script(&call),
                    shows,
                },
            );
            plan.warnings.push(
                "If this write is interrupted, the board may not start from SPI flash any more and may need recovery in maskrom mode."
                    .into(),
            );
            plan.reboot_required = true;
            Ok(plan)
        }
        Target::Disk(dev) => {
            if !u.has("write_uboot_platform") {
                bail!("{pkg} has no write_uboot_platform function");
            }
            let disk = storage::find_disk(root, dev)
                .with_context(|| format!("{dev} is not a disk on this system"))?;
            if !disk.name.starts_with("mmcblk") {
                bail!(
                    "{dev} is a {} device; U-Boot is written only to whole SD or eMMC devices \
                     (/dev/mmcblkN). The boot ROM does not load U-Boot from NVMe, USB or SATA",
                    disk.kind
                );
            }
            if disk.holds_root && !allow_booted {
                bail!(
                    "{dev} is the device the running system booted from. Writing U-Boot there replaces \
                     the bootloader this system starts with. If that is what you want, add --allow-booted-device"
                );
            }
            let call = format!("write_uboot_platform \"$DIR\" {dev}");
            let shows = expand(root, &call)?;
            let first = planned_start.or(disk.partitions.first().map(|p| p.start_bytes));
            for s in &shows {
                if let (Some(end), Some(start)) = (dd_end(root, s, dev), first) {
                    if end > start {
                        bail!(
                            "{s} would write up to byte {end}, past the start of the first partition \
                             at byte {start}; refusing to overwrite partition data"
                        );
                    }
                }
            }
            let mut plan = Plan::new(format!("Write U-Boot from {pkg} to {dev} ({})", disk.kind));
            plan.step(
                format!("Write U-Boot to {dev} with {pkg}'s write_uboot_platform"),
                Action::Shell {
                    script: writer_script(&call),
                    shows,
                },
            );
            if disk.partitions.is_empty() && planned_start.is_none() {
                plan.warnings.push(format!(
                    "{dev} has no partitions; U-Boot alone does not make it bootable"
                ));
            }
            if disk.holds_root {
                plan.warnings.push(format!(
                    "{dev} is the device the running system booted from"
                ));
            }
            plan.reboot_required = true;
            Ok(plan)
        }
    }
}
