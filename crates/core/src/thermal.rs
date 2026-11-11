//! Fans and thermal trip points.
//!
//! Neither supported board's device tree (rk3576-rock-4d.dtb,
//! rk3576-armsom-cm5-io.dtb in the Roxora images) has a pwm-fan node: their
//! cooling maps throttle the CPU and GPU. Trip temperatures can change at
//! run time only with CONFIG_THERMAL_WRITABLE_TRIPS, which the edge kernel
//! does not set, so `fan show` reports them and says why they are fixed.

use anyhow::Result;
use serde::Serialize;

use crate::root::Root;

pub const THERMAL: &str = "/sys/class/thermal";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Trip {
    pub index: u32,
    pub kind: String,
    pub temp_mc: i64,
    pub writable: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Zone {
    pub name: String,
    pub kind: String,
    pub temp_mc: Option<i64>,
    pub trips: Vec<Trip>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Cooling {
    pub name: String,
    pub kind: String,
    pub cur_state: u32,
    pub max_state: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub zones: Vec<Zone>,
    pub cooling: Vec<Cooling>,
    /// A fan: a pwm-fan cooling device or node in the device tree.
    pub fan: bool,
    pub writable_trips: bool,
    pub notes: Vec<String>,
}

fn read(root: &Root, p: &str) -> Option<String> {
    root.read_opt(p).map(|s| s.trim().to_string())
}

fn writable(root: &Root, p: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(root.path(p)).is_ok_and(|m| m.permissions().mode() & 0o200 != 0)
}

fn dt_has_fan(root: &Root) -> bool {
    fn walk(root: &Root, dir: &str, depth: u32) -> bool {
        if depth > 3 {
            return false;
        }
        for n in root.list_dir(dir) {
            let p = format!("{dir}/{n}");
            if n == "compatible" {
                if std::fs::read(root.path(&p)).is_ok_and(|b| b.windows(7).any(|w| w == b"pwm-fan"))
                {
                    return true;
                }
            } else if root.path(&p).is_dir() && walk(root, &p, depth + 1) {
                return true;
            }
        }
        false
    }
    ["/sys/firmware/devicetree/base", "/proc/device-tree"]
        .iter()
        .any(|b| root.exists(b) && walk(root, b, 0))
}

pub fn status(root: &Root) -> Result<Status> {
    let mut zones = Vec::new();
    let mut cooling = Vec::new();
    for n in root.list_dir(THERMAL) {
        let b = format!("{THERMAL}/{n}");
        if n.starts_with("thermal_zone") {
            let mut trips = Vec::new();
            for i in 0..16 {
                let Some(t) = read(root, &format!("{b}/trip_point_{i}_temp")) else {
                    break;
                };
                trips.push(Trip {
                    index: i,
                    kind: read(root, &format!("{b}/trip_point_{i}_type")).unwrap_or_default(),
                    temp_mc: t.parse().unwrap_or(0),
                    writable: writable(root, &format!("{b}/trip_point_{i}_temp")),
                });
            }
            zones.push(Zone {
                kind: read(root, &format!("{b}/type")).unwrap_or_default(),
                temp_mc: read(root, &format!("{b}/temp")).and_then(|t| t.parse().ok()),
                trips,
                name: n,
            });
        } else if n.starts_with("cooling_device") {
            cooling.push(Cooling {
                kind: read(root, &format!("{b}/type")).unwrap_or_default(),
                cur_state: read(root, &format!("{b}/cur_state"))
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0),
                max_state: read(root, &format!("{b}/max_state"))
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0),
                name: n,
            });
        }
    }
    let fan = cooling.iter().any(|c| c.kind.contains("fan")) || dt_has_fan(root);
    let writable_trips = zones.iter().any(|z| z.trips.iter().any(|t| t.writable));
    let mut notes = Vec::new();
    if !fan {
        notes.push(
            "No fan: the device tree has no pwm-fan node, so cooling is by lowering the CPU and GPU clocks."
                .into(),
        );
    }
    if !writable_trips && !zones.is_empty() {
        notes.push(
            "Trip temperatures cannot be changed while running: the kernel is built without \
             CONFIG_THERMAL_WRITABLE_TRIPS. They come from the device tree."
                .into(),
        );
    }
    Ok(Status {
        zones,
        cooling,
        fan,
        writable_trips,
        notes,
    })
}
