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

/// One write the U-Boot package's writer would do.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Write {
    /// The command as the writer would run it.
    pub line: String,
    pub file: String,
    pub device: String,
    pub offset: u64,
    pub len: u64,
}

/// Read-only programs a writer may run while hwtool shows what it would do.
const READ_ONLY: &[&str] = &[
    "basename", "dirname", "ls", "stat", "cat", "head", "tail", "grep", "wc", "sort", "tr", "cut",
    "uname", "lsblk", "blkid", "findfs", "true", "false", "sleep", "sync",
];

fn shim_dir() -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "hwtool-shims-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir(&d).with_context(|| format!("cannot create {}", d.display()))?;
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755))?;
    let put = |name: &str, body: &str| -> Result<()> {
        let f = d.join(name);
        std::fs::write(&f, format!("#!/bin/sh\n{body}\n"))?;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    };
    // The two writers hwtool knows how to show.
    put("dd", r#"printf 'dd'; printf ' %s' "$@"; printf '\n'"#)?;
    put(
        "flashcp",
        r#"printf 'flashcp'; printf ' %s' "$@"; printf '\n'"#,
    )?;
    put("dialog", "exit 1")?;
    // find, minus everything that writes or runs programs.
    put(
        "find",
        r#"for a in "$@"; do case "$a" in -delete|-exec|-execdir|-ok|-okdir|-fprint*|-fls) echo "HWTOOL-UNKNOWN find $a"; exit 127;; esac; done
for p in /usr/bin/find /bin/find; do [ -x "$p" ] && exec "$p" "$@"; done; exit 127"#,
    )?;
    for t in READ_ONLY {
        put(
            t,
            &format!(
                r#"for p in /usr/bin/{t} /bin/{t} /usr/sbin/{t} /sbin/{t}; do [ -x "$p" ] && exec "$p" "$@"; done; exit 127"#
            ),
        )?;
    }
    Ok(d)
}

/// Run the writer function and collect what it would write, without
/// writing anything:
///
/// - bash runs in restricted mode: no command names with a slash, no output
///   redirection, no changing PATH, no exec;
/// - PATH holds only shims: `dd` and `flashcp` print their arguments, a few
///   read-only tools pass through, any other command is reported and makes
///   hwtool refuse the write, since it could not show what that command does;
/// - when hwtool runs as root (sudo, pkexec), bash runs as user nobody.
///
/// Every dd and flashcp line must parse completely and name exactly
/// `allowed` devices, or hwtool refuses.
pub fn expand(root: &Root, func_call: &str, allowed: &[String]) -> Result<Vec<Write>> {
    let prefix = if root.is_system() {
        String::new()
    } else {
        root.base().display().to_string()
    };
    let shims = shim_dir()?;
    let script = format!(
        "unset HISTFILE\n\
         source \"$HWTOOL_ROOT{PLATFORM_INSTALL}\"\n\
         DIR=\"$HWTOOL_ROOT$DIR\"\n\
         command_not_found_handle() {{ echo \"HWTOOL-UNKNOWN $1\"; return 127; }}\n\
         PATH=\"$HWTOOL_SHIMS\"\n\
         readonly PATH SHELL ENV BASH_ENV HISTFILE\n\
         readonly -f command_not_found_handle\n\
         set -r\n\
         {func_call}\n"
    );
    let mut cmd = Command::new("bash");
    cmd.arg("--norc")
        .arg("--noprofile")
        .arg("-c")
        .arg(&script)
        .env_clear()
        .env("HWTOOL_ROOT", &prefix)
        .env("HWTOOL_SHIMS", &shims)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null());
    // SAFETY: getuid only reads.
    if unsafe { libc::geteuid() } == 0 {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(65534) != 0
                    || libc::setuid(65534) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let out = cmd.output();
    let _ = std::fs::remove_dir_all(&shims);
    let out = out.context("cannot run bash")?;
    let stdout = String::from_utf8_lossy(&out.stdout).replace(&prefix_or_none(&prefix), "");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let unknown: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("HWTOOL-UNKNOWN "))
        .collect();
    if !unknown.is_empty() {
        bail!(
            "the U-Boot package's writer runs {}, which hwtool cannot show before it runs; refusing",
            unknown.join(", ")
        );
    }
    // Restricted bash reports absolute command paths, output redirection,
    // exec and PATH changes on standard error. Any error output means the
    // writer did something hwtool cannot show: refuse.
    if !stderr.trim().is_empty() {
        bail!(
            "the U-Boot package's writer does something hwtool cannot show before it runs \
             (an absolute command path, an output redirection, exec or an error): {}",
            stderr.trim()
        );
    }
    if !out.status.success() {
        bail!(
            "the U-Boot package's writer refused: {} {}",
            stdout.trim(),
            stderr.trim()
        );
    }
    let mut writes = Vec::new();
    for l in stdout.lines() {
        if l.starts_with("dd ") || l.starts_with("flashcp ") {
            writes.push(parse_write(root, l)?);
        } else if !l.trim().is_empty() {
            crate::out::say(&format!("    writer says: {l}"));
        }
    }
    if writes.is_empty() {
        bail!(
            "the U-Boot package's writer does not write anything: {}",
            stdout.trim()
        );
    }
    for w in &writes {
        if !allowed.contains(&w.device) {
            bail!(
                "'{}' writes to {}, not to {}; refusing",
                w.line,
                w.device,
                allowed.join(" or ")
            );
        }
    }
    Ok(writes)
}

fn prefix_or_none(p: &str) -> String {
    if p.is_empty() {
        "\u{0}never\u{0}".into()
    } else {
        p.to_string()
    }
}

/// A dd size: a number with an optional GNU suffix (c, w, b, K/k, kB/KB, M,
/// MB, G, GB). Anything else (for example `2x512`) is refused.
pub fn parse_size(v: &str) -> Option<u64> {
    let split = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    let (num, suf) = v.split_at(split);
    let n: u64 = num.parse().ok()?;
    let mul: u64 = match suf {
        "" | "c" => 1,
        "w" => 2,
        "b" => 512,
        "K" | "k" | "KiB" => 1 << 10,
        "kB" | "KB" => 1000,
        "M" | "MiB" => 1 << 20,
        "MB" => 1_000_000,
        "G" | "GiB" => 1 << 30,
        "GB" => 1_000_000_000,
        _ => return None,
    };
    n.checked_mul(mul)
}

/// Parse one dd or flashcp line completely, or fail.
pub fn parse_write(root: &Root, line: &str) -> Result<Write> {
    let mut words = line.split_whitespace();
    let tool = words.next().unwrap_or("");
    let args: Vec<&str> = words.collect();
    let file_len = |f: &str| -> Result<u64> {
        Ok(std::fs::metadata(root.path(f))
            .with_context(|| format!("'{line}': {f} does not exist"))?
            .len())
    };
    match tool {
        "dd" => {
            let (mut input, mut output) = (None, None);
            let (mut bs, mut ibs, mut obs) = (None, 512u64, 512u64);
            let (mut seek, mut skip, mut count) = (0u64, 0u64, None);
            let (mut seek_bytes, mut skip_bytes, mut count_bytes, mut pad) =
                (false, false, false, false);
            for a in &args {
                let (k, v) = a
                    .split_once('=')
                    .with_context(|| format!("'{line}': cannot read '{a}'"))?;
                let size =
                    || parse_size(v).with_context(|| format!("'{line}': cannot read size '{v}'"));
                match k {
                    "if" => input = Some(v.to_string()),
                    "of" => output = Some(v.to_string()),
                    "bs" => bs = Some(size()?),
                    "ibs" => ibs = size()?,
                    "obs" => obs = size()?,
                    "seek" | "oseek" => seek = size()?,
                    "skip" | "iseek" => skip = size()?,
                    "count" => count = Some(size()?),
                    "status" => {}
                    "conv" => {
                        for c in v.split(',') {
                            match c {
                                "notrunc" | "fsync" | "fdatasync" | "noerror" => {}
                                "sync" => pad = true,
                                _ => bail!("'{line}': conv={c} is not supported"),
                            }
                        }
                    }
                    "oflag" | "iflag" => {
                        for f in v.split(',') {
                            match (k, f) {
                                ("oflag", "seek_bytes") => seek_bytes = true,
                                ("iflag", "skip_bytes") => skip_bytes = true,
                                ("iflag", "count_bytes") => count_bytes = true,
                                (_, "sync" | "dsync" | "direct" | "fullblock") => {}
                                _ => bail!("'{line}': {k}={f} is not supported"),
                            }
                        }
                    }
                    _ => bail!("'{line}': {k}= is not supported"),
                }
            }
            if let Some(b) = bs {
                ibs = b;
                obs = b;
            }
            if ibs == 0 || obs == 0 {
                bail!("'{line}': block size 0");
            }
            let file = input.with_context(|| format!("'{line}': no if="))?;
            let device = output.with_context(|| format!("'{line}': no of="))?;
            let total = file_len(&file)?;
            let start_in = if skip_bytes { skip } else { skip * ibs };
            let mut len = total.saturating_sub(start_in);
            if let Some(c) = count {
                len = len.min(if count_bytes { c } else { c * ibs });
            }
            if pad {
                len = len.div_ceil(ibs) * ibs;
            }
            let offset = if seek_bytes { seek } else { seek * obs };
            Ok(Write {
                line: line.to_string(),
                file,
                device,
                offset,
                len,
            })
        }
        "flashcp" => {
            let mut pos = Vec::new();
            for a in &args {
                match *a {
                    "-v" | "--verbose" | "-p" | "--partition" | "-A" | "--erase-all" => {}
                    o if o.starts_with('-') => bail!("'{line}': option {o} is not supported"),
                    x => pos.push(x),
                }
            }
            if pos.len() != 2 {
                bail!("'{line}': expected a file and a device");
            }
            let len = file_len(pos[0])?;
            Ok(Write {
                line: line.to_string(),
                file: pos[0].to_string(),
                device: pos[1].to_string(),
                offset: 0,
                len,
            })
        }
        _ => bail!("cannot read '{line}'"),
    }
}

/// A script that zeroes what `writes` wrote to `device`, for cleanup.
pub fn zero_script(writes: &[Write], device: &str) -> (String, Vec<String>) {
    let mut script = String::from("set -e\n");
    let mut shows = Vec::new();
    for w in writes.iter().filter(|w| w.device == device) {
        let cmd = if w.offset % 512 == 0 {
            format!(
                "dd if=/dev/zero of={device} bs=512 seek={} count={} conv=notrunc,fsync status=none",
                w.offset / 512,
                w.len.div_ceil(512)
            )
        } else {
            format!(
                "dd if=/dev/zero of={device} bs=1 seek={} count={} conv=notrunc,fsync status=none",
                w.offset, w.len
            )
        };
        script.push_str(&cmd);
        script.push('\n');
        shows.push(cmd);
    }
    (script, shows)
}

/// A planned bootloader write and the writes it consists of.
pub struct WritePlan {
    pub plan: Plan,
    pub writes: Vec<Write>,
}

pub fn write_plan(root: &Root, target: &Target, allow_booted: bool) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::NO_UBOOT)?;
    Ok(write_plan_with(root, target, allow_booted, None)?.plan)
}

/// Like [`write_plan`]; `planned_start` is the first partition's start in
/// bytes after a repartition that has not happened yet (migration).
pub fn write_plan_with(
    root: &Root,
    target: &Target,
    allow_booted: bool,
    planned_start: Option<u64>,
) -> Result<WritePlan> {
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
            let block = format!("/dev/{}", first.name.replace("mtd", "mtdblock"));
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
                "write_uboot_platform_mtd \"$DIR\" {block} /dev/null \"{}\"{image}",
                list.join(" ")
            );
            let writes = expand(root, &call, &[first.path.clone(), block.clone()])?;
            if writes.iter().any(|w| w.line.starts_with("flashcp")) && !root.has_program("flashcp")
            {
                bail!("flashcp is not installed; install the mtd-utils package");
            }
            for w in &writes {
                if first.size_bytes > 0 && w.offset + w.len > first.size_bytes {
                    bail!(
                        "'{}' writes {} bytes, more than the {} bytes of {}",
                        w.line,
                        w.offset + w.len,
                        first.size_bytes,
                        first.path
                    );
                }
            }
            let devices: Vec<&str> = {
                let mut d: Vec<&str> = writes.iter().map(|w| w.device.as_str()).collect();
                d.dedup();
                d
            };
            let mut plan = Plan::new(format!(
                "Write U-Boot from {pkg} to SPI flash {} ({}, {} bytes)",
                devices.join(" "),
                first.label,
                first.size_bytes
            ));
            plan.step(
                format!(
                    "Write U-Boot to {} with {pkg}'s write_uboot_platform_mtd",
                    devices.join(" ")
                ),
                Action::Shell {
                    script: writer_script(&call),
                    shows: writes.iter().map(|w| w.line.clone()).collect(),
                },
            );
            plan.warnings.push(
                "If this write is interrupted, the board may not start from SPI flash any more and may need recovery in maskrom mode."
                    .into(),
            );
            plan.reboot_required = true;
            Ok(WritePlan { plan, writes })
        }
        Target::Disk(dev) => {
            if !u.has("write_uboot_platform") {
                bail!("{pkg} has no write_uboot_platform function");
            }
            // Fail closed: without knowing which disk the system runs from,
            // hwtool cannot tell whether this write hits it.
            storage::system(root).context("cannot tell which disk the running system uses")?;
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
            let writes = expand(root, &call, std::slice::from_ref(&disk.path))?;
            let first = planned_start.or(disk.partitions.first().map(|p| p.start_bytes));
            for w in &writes {
                let end = w.offset + w.len;
                if let Some(start) = first {
                    if end > start {
                        bail!(
                            "'{}' would write up to byte {end}, past the start of the first partition \
                             at byte {start}; refusing to overwrite partition data",
                            w.line
                        );
                    }
                }
                if disk.size_bytes > 0 && end > disk.size_bytes {
                    bail!("'{}' would write past the end of {dev}", w.line);
                }
            }
            let mut plan = Plan::new(format!("Write U-Boot from {pkg} to {dev} ({})", disk.kind));
            plan.step(
                format!("Write U-Boot to {dev} with {pkg}'s write_uboot_platform"),
                Action::Shell {
                    script: writer_script(&call),
                    shows: writes.iter().map(|w| w.line.clone()).collect(),
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
            Ok(WritePlan { plan, writes })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("32k"), Some(32768));
        assert_eq!(parse_size("32K"), Some(32768));
        assert_eq!(parse_size("1M"), Some(1 << 20));
        assert_eq!(parse_size("2b"), Some(1024));
        assert_eq!(parse_size("1kB"), Some(1000));
        assert_eq!(parse_size("64"), Some(64));
        assert_eq!(parse_size("2x512"), None);
        assert_eq!(parse_size("1T"), None);
        assert_eq!(parse_size(""), None);
    }
}
