//! Saving an installed package as a .deb file, so `kernel revert` can
//! reinstall it when the Armbian repository no longer offers that version
//! (or never did, as for some community boards' U-Boot packages).
//!
//! If apt still has the original file in /var/cache/apt/archives, that file
//! is copied: it is the package exactly as it was installed. Otherwise the
//! package is rebuilt from the dpkg database the way dpkg-repack does it,
//! without needing dpkg-repack: the control fields from
//! /var/lib/dpkg/status (minus the install state), the maintainer scripts,
//! conffiles and md5sums from /var/lib/dpkg/info, and the installed files
//! listed in /var/lib/dpkg/info/<package>.list, packed with `dpkg-deb`.
//! Files are owned by root in the rebuilt package; the kernel, device tree,
//! header and U-Boot packages only contain root-owned files.

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::exec::Executor;
use crate::root::Root;

pub const PACKAGES_DIR: &str = "/var/lib/hwtool/packages";
const APT_CACHE: &str = "/var/cache/apt/archives";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedPackage {
    pub name: String,
    pub version: String,
    /// Path of the saved .deb on the target system.
    pub file: String,
}

/// The status paragraph of an installed package.
pub fn status_paragraph(root: &Root, name: &str) -> Option<String> {
    let text = root.read_opt("/var/lib/dpkg/status")?;
    text.split("\n\n")
        .find(|p| {
            p.lines().any(|l| l == format!("Package: {name}"))
                && p.lines().any(|l| l == "Status: install ok installed")
        })
        .map(str::to_string)
}

fn field(para: &str, key: &str) -> Option<String> {
    para.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
        .map(|v| v.trim().to_string())
}

/// apt's file name for a package: an epoch colon becomes %3a.
pub fn deb_name(name: &str, version: &str, arch: &str) -> String {
    format!("{name}_{}_{arch}.deb", version.replace(':', "%3a"))
}

/// Where a package will be saved, and whether apt's cached copy exists.
pub struct SavePlan {
    pub file: String,
    pub from_cache: Option<String>,
    pub installed_kib: u64,
}

pub fn plan_save(root: &Root, name: &str, version: &str, dir: &str) -> Result<SavePlan> {
    let para = status_paragraph(root, name)
        .with_context(|| format!("{name} is not installed, so it cannot be saved"))?;
    let arch = field(&para, "Architecture").unwrap_or_else(|| "all".into());
    let deb = deb_name(name, version, &arch);
    let cache = format!("{APT_CACHE}/{deb}");
    Ok(SavePlan {
        file: format!("{dir}/{deb}"),
        from_cache: root.exists(&cache).then_some(cache),
        installed_kib: field(&para, "Installed-Size")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    })
}

fn info_file(root: &Root, name: &str, arch: &str, suffix: &str) -> Option<String> {
    [
        format!("/var/lib/dpkg/info/{name}:{arch}.{suffix}"),
        format!("/var/lib/dpkg/info/{name}.{suffix}"),
    ]
    .into_iter()
    .find(|p| root.exists(p))
}

/// The control file: the status paragraph without the fields that describe
/// the installation rather than the package.
fn control(para: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in para.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let key = line.split(':').next().unwrap_or("");
            skip = matches!(key, "Status" | "Config-Version" | "Conffiles");
        }
        if !skip {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Save `name` (installed at `version`) into `dir` and return the .deb path.
pub fn save(
    root: &Root,
    exec: &mut dyn Executor,
    name: &str,
    version: &str,
    dir: &str,
) -> Result<String> {
    let para = status_paragraph(root, name).with_context(|| format!("{name} is not installed"))?;
    if field(&para, "Version").as_deref() != Some(version) {
        bail!("{name} {version} is no longer the installed version");
    }
    let arch = field(&para, "Architecture").unwrap_or_else(|| "all".into());
    let plan = plan_save(root, name, version, dir)?;
    fs::create_dir_all(root.path(dir)).with_context(|| format!("cannot create {dir}"))?;
    if let Some(cache) = &plan.from_cache {
        fs::copy(root.path(cache), root.path(&plan.file))
            .with_context(|| format!("cannot copy {cache}"))?;
        return Ok(plan.file);
    }

    let staging = format!("{dir}/.staging-{name}");
    let st = root.path(&staging);
    if st.exists() {
        fs::remove_dir_all(&st)?;
    }
    fs::create_dir_all(st.join("DEBIAN"))?;
    fs::write(st.join("DEBIAN/control"), control(&para))?;
    for s in ["preinst", "postinst", "prerm", "postrm", "config"] {
        if let Some(f) = info_file(root, name, &arch, s) {
            let dst = st.join("DEBIAN").join(s);
            fs::copy(root.path(&f), &dst)?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))?;
        }
    }
    for s in ["triggers", "shlibs", "symbols", "templates", "md5sums"] {
        if let Some(f) = info_file(root, name, &arch, s) {
            let dst = st.join("DEBIAN").join(s);
            fs::copy(root.path(&f), &dst)?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o644))?;
        }
    }
    let conffiles: Vec<String> = para
        .lines()
        .skip_while(|l| !l.starts_with("Conffiles:"))
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    if !conffiles.is_empty() {
        fs::write(st.join("DEBIAN/conffiles"), conffiles.join("\n") + "\n")?;
    }

    let list_file = info_file(root, name, &arch, "list")
        .with_context(|| format!("dpkg has no file list for {name}"))?;
    let mut missing = Vec::new();
    for path in root.read(&list_file)?.lines() {
        if path.is_empty() || path == "/." {
            continue;
        }
        let src = root.path(path);
        let dst = st.join(path.trim_start_matches('/'));
        let meta = match fs::symlink_metadata(&src) {
            Ok(m) => m,
            Err(_) => {
                missing.push(path.to_string());
                continue;
            }
        };
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&src)?, &dst)?;
        } else if meta.is_dir() {
            fs::create_dir_all(&dst)?;
            fs::set_permissions(&dst, fs::Permissions::from_mode(meta.permissions().mode()))?;
        } else {
            fs::copy(&src, &dst).with_context(|| format!("cannot copy {path}"))?;
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "    {name}: {} packaged file(s) no longer exist and are left out: {}",
            missing.len(),
            missing.join(" ")
        );
    }
    exec.run(&[
        "dpkg-deb".into(),
        "--root-owner-group".into(),
        "--build".into(),
        st.display().to_string(),
        root.path(&plan.file).display().to_string(),
    ])?;
    fs::remove_dir_all(&st)?;
    Ok(plan.file)
}

/// Remove saved package sets other than `keep`.
pub fn prune(root: &Root, keep: &str) -> Result<()> {
    for d in root.list_dir(PACKAGES_DIR) {
        let p = format!("{PACKAGES_DIR}/{d}");
        if p != keep {
            fs::remove_dir_all(root.path(&p)).with_context(|| format!("cannot remove {p}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_drops_install_state() {
        let para = "Package: a\nStatus: install ok installed\nVersion: 1\nConffiles:\n /etc/a 0123\nDescription: x\n more\n";
        assert_eq!(
            control(para),
            "Package: a\nVersion: 1\nDescription: x\n more\n"
        );
    }

    #[test]
    fn epoch_in_file_name() {
        assert_eq!(deb_name("a", "1:2.0", "arm64"), "a_1%3a2.0_arm64.deb");
    }
}
