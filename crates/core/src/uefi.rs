//! UEFI images (roxora-os/build `generic-rk3576-uefi`).
//!
//! Such an image has no U-Boot package, no boot.scr and no armbianEnv.txt.
//! The board's UEFI firmware (for example edk2-rk3576) starts GRUB
//! (grub-efi-arm64) from the EFI system partition at /boot/efi, and the
//! firmware gives the kernel its device tree: by default its own, or, with
//! its "DTB override" option, one of Armbian's rk3576 DTBs that
//! /etc/kernel/postinst.d/zz-roxora-efi-dtb copies to
//! /boot/efi/dtb/rockchip/. Everything hwtool does through boot.scr, U-Boot
//! or the vendor kernel is refused there with the reason.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::root::Root;

pub const ESP: &str = "/boot/efi";
pub const ESP_DTB_DIR: &str = "/boot/efi/dtb/rockchip";
pub const OVERLAYS: &str =
    "UEFI image: device tree overlays are applied by the firmware, not by hwtool; \
     see 'UEFI images' in hwtool's docs/how-it-works.md";

/// True on a UEFI image: the board name says so, or there is no U-Boot
/// package and the image boots GRUB from an EFI system partition.
pub fn is_uefi(root: &Root) -> bool {
    let board = root
        .read_opt("/etc/armbian-release")
        .unwrap_or_default()
        .lines()
        .find_map(|l| {
            l.strip_prefix("BOARD=")
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_default();
    if board.ends_with("-uefi") {
        return true;
    }
    let no_uboot = !root.exists(crate::bootloader::PLATFORM_INSTALL)
        && !crate::dpkg::installed(root)
            .iter()
            .any(|p| p.name.starts_with("linux-u-boot-"));
    let esp = root.exists(&format!("{ESP}/EFI"));
    let grub = root.exists("/boot/grub/grub.cfg") || root.has_program("update-grub");
    no_uboot && esp && grub
}

/// Refuse `what` on a UEFI image, with `why`.
pub fn refuse(root: &Root, why: &str) -> Result<()> {
    if is_uefi(root) {
        bail!("{why}");
    }
    Ok(())
}

pub const NO_UBOOT: &str =
    "UEFI image: there is no U-Boot; the board starts from its UEFI firmware, which hwtool \
     does not write";
pub const NO_BOOTENV: &str =
    "UEFI image: there is no boot.scr or armbianEnv.txt; kernel arguments are in \
     /etc/default/grub.d/ (run update-grub after changing them)";
pub const NO_MIGRATE: &str =
    "UEFI image: moving the system needs a U-Boot boot script; on a UEFI image copy it \
     with the firmware's boot manager in mind (not supported by hwtool yet)";
pub const NO_SWITCH: &str =
    "UEFI image: only the edge kernel boots with UEFI here; the vendor branch has no UEFI support";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub uefi: bool,
    /// /sys/firmware/efi exists: the running system was started by UEFI.
    pub booted_with_uefi: bool,
    pub firmware_vendor: Option<String>,
    pub firmware_version: Option<String>,
    pub firmware_date: Option<String>,
    /// fw_platform_size (64 or 32).
    pub platform_size: Option<String>,
    /// /proc/device-tree/model and compatible.
    pub model: Option<String>,
    pub compatible: Vec<String>,
    pub soc: Option<String>,
    /// "firmware", "override" or "unknown".
    pub device_tree: String,
    /// The ESP DTB the running tree matches, if any.
    pub override_file: Option<String>,
    /// Share of the running tree's properties that the best ESP DTB has
    /// with the same value (0 to 1).
    pub similarity: Option<f64>,
    pub esp_dtbs: Vec<String>,
    pub notes: Vec<String>,
}

fn trim(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().trim_end_matches('\0').to_string())
        .filter(|v| !v.is_empty())
}

fn strings(b: &[u8]) -> Vec<String> {
    b.split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

type Props = BTreeMap<String, Vec<u8>>;

/// Nodes skipped when comparing: what firmware and kernel fill in at boot.
fn skipped(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    let name = first.split('@').next().unwrap_or("");
    matches!(
        name,
        "chosen" | "memory" | "reserved-memory" | "__symbols__" | "__fixups__" | "__local_fixups__"
    )
}

/// Every property of a flattened device tree, as "/path/to/node/prop".
pub fn parse_fdt(b: &[u8]) -> Option<Props> {
    let be =
        |o: usize| -> Option<u32> { Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    if be(0)? != 0xd00dfeed {
        return None;
    }
    let off_struct = be(8)? as usize;
    let off_strings = be(12)? as usize;
    let mut props = Props::new();
    let mut path: Vec<String> = Vec::new();
    let mut i = off_struct;
    loop {
        let tok = be(i)?;
        i += 4;
        match tok {
            1 => {
                let end = i + b.get(i..)?.iter().position(|c| *c == 0)?;
                let name = String::from_utf8_lossy(&b[i..end]).into_owned();
                i = (end + 1).div_ceil(4) * 4;
                path.push(name);
            }
            2 => {
                path.pop();
            }
            3 => {
                let len = be(i)? as usize;
                let nameoff = be(i + 4)? as usize;
                let val = b.get(i + 8..i + 8 + len)?.to_vec();
                i = (i + 8 + len).div_ceil(4) * 4;
                let s = off_strings + nameoff;
                let end = s + b.get(s..)?.iter().position(|c| *c == 0)?;
                let pname = String::from_utf8_lossy(&b[s..end]).into_owned();
                let p = format!("{}/{pname}", path[1..].join("/"));
                props.insert(format!("/{}", p.trim_start_matches('/')), val);
            }
            4 => {}
            9 => break,
            _ => return None,
        }
    }
    Some(props)
}

/// The running tree from /proc/device-tree, in the same form.
fn running(root: &Root) -> Props {
    fn walk(root: &Root, base: &str, rel: &str, out: &mut Props, depth: u32) {
        if depth > 12 {
            return;
        }
        for n in root.list_dir(&format!("{base}{rel}")) {
            let r = format!("{rel}/{n}");
            let p = root.path(&format!("{base}{r}"));
            if p.is_dir() {
                walk(root, base, &r, out, depth + 1);
            } else if let Ok(v) = std::fs::read(&p) {
                out.insert(r, v);
            }
        }
    }
    let mut out = Props::new();
    walk(root, "/proc/device-tree", "", &mut out, 0);
    out
}

/// Share of `run`'s properties (outside skipped nodes, without phandles,
/// which dtc and firmware number differently) that `dtb` has with the same
/// value.
fn similarity(run: &Props, dtb: &Props) -> f64 {
    let keep = |k: &String| {
        !skipped(k) && !k.ends_with("/phandle") && !k.ends_with("/linux,phandle") && k != "/name"
    };
    let total = run.keys().filter(|k| keep(k)).count();
    if total == 0 {
        return 0.0;
    }
    let same = run
        .iter()
        .filter(|(k, v)| keep(k) && dtb.get(*k) == Some(*v))
        .count();
    same as f64 / total as f64
}

pub fn status(root: &Root) -> Status {
    let efi = "/sys/firmware/efi";
    let dmi = |f: &str| trim(root.read_opt(&format!("/sys/class/dmi/id/{f}")));
    let compatible =
        strings(&std::fs::read(root.path("/proc/device-tree/compatible")).unwrap_or_default());
    let soc = compatible
        .iter()
        .find(|c| c.starts_with("rockchip,rk"))
        .map(|c| c.trim_start_matches("rockchip,").to_string());
    let mut notes = Vec::new();
    let mut esp_dtbs: Vec<String> = root
        .list_dir(ESP_DTB_DIR)
        .into_iter()
        .filter(|n| n.ends_with(".dtb"))
        .map(|n| format!("{ESP_DTB_DIR}/{n}"))
        .collect();
    esp_dtbs.sort();
    let run = running(root);
    let (mut device_tree, mut override_file, mut sim) = ("unknown".to_string(), None, None);
    if run.is_empty() {
        notes.push("/proc/device-tree is empty: the kernel got no device tree.".into());
    } else {
        let mut best: Option<(f64, String)> = None;
        for f in &esp_dtbs {
            let Ok(b) = std::fs::read(root.path(f)) else {
                continue;
            };
            if let Some(p) = parse_fdt(&b) {
                let s = similarity(&run, &p);
                if best.as_ref().is_none_or(|(bs, _)| s > *bs) {
                    best = Some((s, f.clone()));
                }
            }
        }
        match best {
            // The firmware adds /chosen and memory nodes and may patch a few
            // properties even when it passes Armbian's DTB.
            Some((s, f)) if s >= 0.98 => {
                device_tree = "override".into();
                override_file = Some(f);
                sim = Some(s);
            }
            Some((s, _)) => {
                device_tree = "firmware".into();
                sim = Some(s);
            }
            None => {
                device_tree = "firmware".into();
                notes.push(format!(
                    "No DTBs in {ESP_DTB_DIR} to compare with, so the device tree is taken to be the firmware's."
                ));
            }
        }
    }
    Status {
        uefi: is_uefi(root),
        booted_with_uefi: root.exists(efi),
        firmware_vendor: dmi("bios_vendor"),
        firmware_version: dmi("bios_version"),
        firmware_date: dmi("bios_date"),
        platform_size: trim(root.read_opt(&format!("{efi}/fw_platform_size"))),
        model: trim(root.read_opt("/proc/device-tree/model")),
        compatible,
        soc,
        device_tree,
        override_file,
        similarity: sim,
        esp_dtbs,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny FDT: / { compatible; model; cpus { x; }; chosen { bootargs; }; }
    fn fdt(model: &str) -> Vec<u8> {
        let mut st = Vec::new();
        let strs = b"compatible\0model\0x\0bootargs\0";
        let node = |st: &mut Vec<u8>, n: &str| {
            st.extend(1u32.to_be_bytes());
            let mut b = n.as_bytes().to_vec();
            b.push(0);
            while !b.len().is_multiple_of(4) {
                b.push(0);
            }
            st.extend(b);
        };
        let prop = |st: &mut Vec<u8>, off: u32, v: &[u8]| {
            st.extend(3u32.to_be_bytes());
            st.extend((v.len() as u32).to_be_bytes());
            st.extend(off.to_be_bytes());
            let mut b = v.to_vec();
            while !b.len().is_multiple_of(4) {
                b.push(0);
            }
            st.extend(b);
        };
        node(&mut st, "");
        prop(&mut st, 0, b"radxa,rock-4d\0rockchip,rk3576\0");
        prop(&mut st, 11, format!("{model}\0").as_bytes());
        node(&mut st, "cpus");
        prop(&mut st, 17, b"1\0");
        st.extend(2u32.to_be_bytes());
        node(&mut st, "chosen");
        prop(&mut st, 19, b"console=x\0");
        st.extend(2u32.to_be_bytes());
        st.extend(2u32.to_be_bytes());
        st.extend(9u32.to_be_bytes());
        let mut h = Vec::new();
        let off_struct = 40u32;
        let off_strings = off_struct + st.len() as u32;
        for v in [
            0xd00dfeed,
            off_strings + strs.len() as u32,
            off_struct,
            off_strings,
            40,
            17,
            16,
            0,
            strs.len() as u32,
            st.len() as u32,
        ] {
            h.extend(v.to_be_bytes());
        }
        h.extend(st);
        h.extend(strs);
        h
    }

    #[test]
    fn parse_and_compare() {
        let p = parse_fdt(&fdt("Radxa ROCK 4D")).unwrap();
        assert_eq!(
            p.get("/model").map(|v| v.as_slice()),
            Some(&b"Radxa ROCK 4D\0"[..])
        );
        assert!(p.contains_key("/cpus/x") && p.contains_key("/chosen/bootargs"));
        let q = parse_fdt(&fdt("Other")).unwrap();
        assert_eq!(similarity(&p, &p), 1.0);
        assert!(similarity(&p, &q) < 1.0);
    }
}
