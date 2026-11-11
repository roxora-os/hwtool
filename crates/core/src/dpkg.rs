//! Read installed packages from the dpkg status file, and candidates from
//! `apt-cache policy` output.

use crate::root::Root;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// Installed packages: status `install ok installed`, or `hold ok
/// installed` for packages held with apt-mark.
pub fn installed(root: &Root) -> Vec<Package> {
    let text = root.read_opt("/var/lib/dpkg/status").unwrap_or_default();
    parse_status(&text)
}

/// Names of installed packages that are held (apt-mark hold).
pub fn held(root: &Root) -> Vec<String> {
    let text = root.read_opt("/var/lib/dpkg/status").unwrap_or_default();
    text.split("\n\n")
        .filter(|p| p.lines().any(|l| l == "Status: hold ok installed"))
        .filter_map(|p| {
            p.lines()
                .find_map(|l| l.strip_prefix("Package: "))
                .map(|n| n.trim().to_string())
        })
        .collect()
}

pub fn parse_status(text: &str) -> Vec<Package> {
    let mut out = Vec::new();
    for para in text.split("\n\n") {
        let mut name = None;
        let mut version = None;
        let mut ok = false;
        for line in para.lines() {
            if let Some(v) = line.strip_prefix("Package: ") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Version: ") {
                version = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Status: ") {
                ok = matches!(v.trim(), "install ok installed" | "hold ok installed");
            }
        }
        if let (Some(name), Some(version), true) = (name, version, ok) {
            out.push(Package { name, version });
        }
    }
    out
}

/// What `apt-cache policy <pkg>` says about one package.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub installed: Option<String>,
    pub candidate: Option<String>,
    pub versions: Vec<String>,
}

/// Parse the output of `apt-cache policy` for one package.
pub fn parse_policy(text: &str) -> Policy {
    let mut p = Policy::default();
    let mut in_table = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Installed:") {
            p.installed = none_if_none(v);
        } else if let Some(v) = t.strip_prefix("Candidate:") {
            p.candidate = none_if_none(v);
        } else if t.starts_with("Version table:") {
            in_table = true;
        } else if in_table {
            let t = t.trim_start_matches("***").trim();
            let fields: Vec<&str> = t.split_whitespace().collect();
            // Version lines are "<version> <priority>"; source lines start
            // with the priority.
            if fields.len() == 2
                && fields[1].parse::<i32>().is_ok()
                && fields[0].parse::<i32>().is_err()
            {
                p.versions.push(fields[0].to_string());
            }
        }
    }
    p
}

fn none_if_none(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() || v == "(none)" {
        None
    } else {
        Some(v.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_only_installed() {
        let s = "Package: a\nStatus: install ok installed\nVersion: 1\n\nPackage: b\nStatus: deinstall ok config-files\nVersion: 2\n";
        assert_eq!(
            parse_status(s),
            vec![Package {
                name: "a".into(),
                version: "1".into()
            }]
        );
    }

    #[test]
    fn policy() {
        let s = "linux-image-vendor-rk35xx:\n  Installed: (none)\n  Candidate: 26.5.1\n  Version table:\n     26.5.1 500\n        500 http://apt.armbian.com noble/main arm64 Packages\n     25.11.2 500\n        500 http://apt.armbian.com noble/main arm64 Packages\n";
        let p = parse_policy(s);
        assert_eq!(p.installed, None);
        assert_eq!(p.candidate.as_deref(), Some("26.5.1"));
        assert_eq!(p.versions, vec!["26.5.1", "25.11.2"]);
        let none = parse_policy("x:\n  Installed: (none)\n  Candidate: (none)\n  Version table:\n");
        assert_eq!(none.candidate, None);
    }
}
