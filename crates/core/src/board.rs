//! Which board this is and which kernel branches it supports.
//!
//! The source is `/etc/armbian-release`, written by the board support
//! package. If it is missing, the board is recognised from the installed
//! `linux-u-boot-<board>-<branch>` package and a built-in table of the
//! boards Roxora builds (`boards.conf` in roxora-os/build).

use anyhow::{bail, Result};
use serde::Serialize;

use crate::dpkg;
use crate::root::Root;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Board {
    /// Armbian board name, for example `radxa-rock-4d`.
    pub board: String,
    pub board_name: String,
    /// Armbian BOARDFAMILY, for example `rk35xx`.
    pub board_family: String,
    /// SoC, for example `rk3576`.
    pub soc: String,
    /// Kernel branches the board supports (KERNEL_TARGET).
    pub branches: Vec<String>,
    /// Where the information came from.
    pub source: String,
}

struct Known {
    board: &'static str,
    name: &'static str,
    family: &'static str,
    soc: &'static str,
    branches: &'static [&'static str],
}

/// Read from config/boards/radxa-rock-4d.conf and armsom-cm5-io.csc at the
/// armbian/build commit Roxora pins.
const KNOWN: &[Known] = &[
    Known {
        board: "radxa-rock-4d",
        name: "Radxa ROCK 4D",
        family: "rk35xx",
        soc: "rk3576",
        branches: &["vendor", "edge"],
    },
    Known {
        board: "armsom-cm5-io",
        name: "ArmSoM CM5-IO",
        family: "rk35xx",
        soc: "rk3576",
        branches: &["vendor", "edge"],
    },
];

fn parse_release(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

impl Board {
    pub fn detect(root: &Root) -> Result<Board> {
        if let Some(text) = root.read_opt("/etc/armbian-release") {
            let kv = parse_release(&text);
            let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
            if let Some(board) = get("BOARD") {
                let known = KNOWN.iter().find(|k| k.board == board);
                let branches: Vec<String> = get("KERNEL_TARGET")
                    .map(|v| {
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .filter(|v: &Vec<String>| !v.is_empty())
                    .or_else(|| known.map(|k| k.branches.iter().map(|s| s.to_string()).collect()))
                    .unwrap_or_default();
                return Ok(Board {
                    board_name: known
                        .map(|k| k.name.to_string())
                        .or_else(|| get("BOARD_NAME"))
                        .unwrap_or_else(|| board.clone()),
                    board_family: get("BOARDFAMILY")
                        .or_else(|| known.map(|k| k.family.to_string()))
                        .unwrap_or_default(),
                    soc: get("BOOT_SOC")
                        .or_else(|| known.map(|k| k.soc.to_string()))
                        .unwrap_or_default(),
                    branches,
                    board,
                    source: "/etc/armbian-release".into(),
                });
            }
        }
        for pkg in dpkg::installed(root) {
            if let Some(rest) = pkg.name.strip_prefix("linux-u-boot-") {
                for k in KNOWN {
                    if rest
                        .strip_prefix(k.board)
                        .is_some_and(|b| b.starts_with('-'))
                    {
                        return Ok(Board {
                            board: k.board.into(),
                            board_name: k.name.into(),
                            board_family: k.family.into(),
                            soc: k.soc.into(),
                            branches: k.branches.iter().map(|s| s.to_string()).collect(),
                            source: format!("installed package {}", pkg.name),
                        });
                    }
                }
            }
        }
        bail!(
            "cannot tell which board this is: /etc/armbian-release is missing and no known \
             linux-u-boot-<board>-<branch> package is installed"
        )
    }

    /// LINUXFAMILY of a branch, which is part of the kernel package names.
    /// Rockchip boards use the shared `rockchip64` family for the mainline
    /// branches and their BOARDFAMILY for vendor kernels
    /// (config/sources/families/include/rockchip64_common.inc, rk35xx.conf).
    pub fn family_for(&self, branch: &str) -> String {
        if branch.starts_with("vendor") || branch == "legacy" {
            self.board_family.clone()
        } else if self.board_family == "rk35xx" || self.board_family == "rockchip64" {
            "rockchip64".into()
        } else {
            self.board_family.clone()
        }
    }

    pub fn image_package(&self, branch: &str) -> String {
        format!("linux-image-{branch}-{}", self.family_for(branch))
    }

    pub fn dtb_package(&self, branch: &str) -> String {
        format!("linux-dtb-{branch}-{}", self.family_for(branch))
    }

    /// linux-headers-<branch>-<family> (kernel-debs.sh).
    pub fn headers_package(&self, branch: &str) -> String {
        format!("linux-headers-{branch}-{}", self.family_for(branch))
    }

    /// The U-Boot package puts the board before the branch.
    pub fn uboot_package(&self, branch: &str) -> String {
        format!("linux-u-boot-{}-{branch}", self.board)
    }

    pub fn supports(&self, branch: &str) -> bool {
        self.branches.iter().any(|b| b == branch)
    }

    /// The SoC overlay prefixes Armbian uses for a branch:
    /// `rockchip-<soc>` for mainline kernels, `<soc>` for vendor kernels
    /// (`rockchip_soc_overlay_prefixes` in rockchip64_common.inc).
    pub fn soc_prefixes(&self, branch: &str) -> Vec<String> {
        let base = if branch.starts_with("vendor") {
            ""
        } else {
            "rockchip-"
        };
        let socs: Vec<&str> = match self.soc.as_str() {
            "rk3566" => vec!["rk3566", "rk3568"],
            "rk3568" => vec!["rk3568", "rk3566"],
            "rk3399pro" => vec!["rk3399"],
            "rk3588s" => vec!["rk3588"],
            s if s.starts_with("rk") => vec![s],
            _ => return vec!["rockchip".into()],
        };
        socs.iter().map(|s| format!("{base}{s}")).collect()
    }

    /// overlay_prefix for `branch`, keeping the board's own prefixes
    /// (OVERLAY_PREFIX_BOARD, listed first) from the current value.
    pub fn overlay_prefix_for(&self, branch: &str, current: &[String]) -> Vec<String> {
        let all_soc: Vec<String> = self
            .branches
            .iter()
            .chain(["edge".to_string(), "vendor".to_string()].iter())
            .flat_map(|b| self.soc_prefixes(b))
            .collect();
        let mut out: Vec<String> = current
            .iter()
            .filter(|p| !all_soc.contains(p))
            .cloned()
            .collect();
        for p in self.soc_prefixes(branch) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rock4d() -> Board {
        Board {
            board: "radxa-rock-4d".into(),
            board_name: "Radxa ROCK 4D".into(),
            board_family: "rk35xx".into(),
            soc: "rk3576".into(),
            branches: vec!["vendor".into(), "edge".into()],
            source: "test".into(),
        }
    }

    #[test]
    fn package_names() {
        let b = rock4d();
        assert_eq!(b.image_package("edge"), "linux-image-edge-rockchip64");
        assert_eq!(b.dtb_package("vendor"), "linux-dtb-vendor-rk35xx");
        assert_eq!(
            b.uboot_package("vendor"),
            "linux-u-boot-radxa-rock-4d-vendor"
        );
    }

    #[test]
    fn overlay_prefix_switch_keeps_board_prefix() {
        let b = rock4d();
        let cur = vec!["armsom-cm5-io".to_string(), "rockchip-rk3576".to_string()];
        assert_eq!(
            b.overlay_prefix_for("vendor", &cur),
            vec!["armsom-cm5-io", "rk3576"]
        );
        let back = b.overlay_prefix_for("edge", &b.overlay_prefix_for("vendor", &cur));
        assert_eq!(back, cur);
        assert_eq!(
            b.overlay_prefix_for("vendor", &["rockchip-rk3576".into()]),
            vec!["rk3576"]
        );
    }
}
