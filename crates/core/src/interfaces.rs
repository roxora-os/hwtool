//! Pin header interfaces (I2C, UART, SPI with spidev, PWM, 1-Wire) through
//! overlays hwtool ships.
//!
//! Each board's overlays and index.json are in
//! /usr/share/hwtool/interfaces/<board>/ (built from data/interfaces in the
//! source, see tools/gen-interfaces.py for where every pin fact comes from).
//! Enabling one copies its .dtbo to /boot/overlay-user/hwtool-<name>.dtbo and
//! adds hwtool-<name> to user_overlays=, which boot.scr applies.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::env::BootEnv;
use crate::overlays::USER_OVERLAY_DIR;
use crate::plan::{Action, Plan};
use crate::root::Root;
use crate::ENV_FILE;

pub const DATA_DIR: &str = "/usr/share/hwtool/interfaces";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pin {
    pub pin: u32,
    pub signal: String,
    pub gpio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Interface {
    pub name: String,
    pub kind: String,
    pub title: String,
    pub node: String,
    #[serde(default)]
    pub groups: Vec<String>,
    pub pins: Vec<Pin>,
}

#[derive(Debug, Clone, Deserialize)]
struct Index {
    board: String,
    busy: std::collections::BTreeMap<String, String>,
    interfaces: Vec<Interface>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    #[serde(flatten)]
    pub interface: Interface,
    pub enabled: bool,
    /// Other interfaces it cannot be enabled with (shared pins or the same
    /// controller).
    pub conflicts: Vec<String>,
    /// Enabled interfaces among those conflicts.
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Status {
    pub board: String,
    pub interfaces: Vec<Entry>,
    /// Header pins the board itself uses: pin -> what for.
    pub busy: Vec<(u32, String)>,
}

fn overlay_name(name: &str) -> String {
    format!("hwtool-{name}")
}

fn load(root: &Root) -> Result<(Board, Index)> {
    let board = Board::detect(root)?;
    let path = format!("{DATA_DIR}/{}/index.json", board.board);
    let text = root.read_opt(&path).with_context(|| {
        format!(
            "hwtool has no pin header interfaces for {} ({path} is missing)",
            board.board_name
        )
    })?;
    let idx: Index = serde_json::from_str(&text).with_context(|| format!("cannot read {path}"))?;
    if idx.board != board.board {
        bail!("{path} is for {}, not {}", idx.board, board.board);
    }
    Ok((board, idx))
}

fn clash(a: &Interface, b: &Interface) -> bool {
    a.name != b.name
        && (a.node == b.node && a.kind != "w1"
            || a.pins.iter().any(|p| b.pins.iter().any(|q| q.pin == p.pin)))
}

pub fn status(root: &Root) -> Result<Status> {
    crate::uefi::refuse(root, crate::uefi::OVERLAYS)?;
    let (board, idx) = load(root)?;
    let env = BootEnv::parse(&root.read(ENV_FILE)?);
    let users = env.list("user_overlays");
    let on = |i: &Interface| {
        users.contains(&overlay_name(&i.name))
            && root.exists(&format!(
                "{USER_OVERLAY_DIR}/{}.dtbo",
                overlay_name(&i.name)
            ))
    };
    let interfaces = idx
        .interfaces
        .iter()
        .map(|i| {
            let conflicts: Vec<String> = idx
                .interfaces
                .iter()
                .filter(|o| clash(i, o))
                .map(|o| o.name.clone())
                .collect();
            let blocked_by = idx
                .interfaces
                .iter()
                .filter(|o| clash(i, o) && on(o))
                .map(|o| o.name.clone())
                .collect();
            Entry {
                enabled: on(i),
                interface: i.clone(),
                conflicts,
                blocked_by,
            }
        })
        .collect();
    let mut busy: Vec<(u32, String)> = idx
        .busy
        .iter()
        .filter_map(|(k, v)| k.parse().ok().map(|p| (p, v.clone())))
        .collect();
    busy.sort();
    Ok(Status {
        board: board.board,
        interfaces,
        busy,
    })
}

fn pin_list(i: &Interface) -> String {
    i.pins
        .iter()
        .map(|p| format!("{} {}", p.pin, p.signal))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn enable_plan(root: &Root, names: &[String]) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::OVERLAYS)?;
    let st = status(root)?;
    let mut wanted: Vec<&Entry> = Vec::new();
    for n in names {
        let e = st
            .interfaces
            .iter()
            .find(|e| &e.interface.name == n)
            .with_context(|| {
                format!(
                    "no interface {n}; there are {}",
                    st.interfaces
                        .iter()
                        .map(|e| e.interface.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        if !e.blocked_by.is_empty() {
            bail!(
                "{n} uses pins or the controller of {}, which is enabled; disable that first",
                e.blocked_by.join(", ")
            );
        }
        if let Some(o) = wanted.iter().find(|o| clash(&o.interface, &e.interface)) {
            bail!(
                "{n} and {} share pins or a controller; enable only one of them",
                o.interface.name
            );
        }
        wanted.push(e);
    }
    let before = BootEnv::parse(&root.read(ENV_FILE)?);
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    let mut p = Plan::new(format!("Enable pin header interfaces: {}", names.join(" ")));
    for e in wanted.iter().filter(|e| !e.enabled) {
        let n = overlay_name(&e.interface.name);
        let src = format!("{DATA_DIR}/{}/{}.dtbo", st.board, e.interface.name);
        if !root.exists(&src) {
            bail!("{src} is missing from the hwtool package");
        }
        p.run(
            format!(
                "Install {} (pins {})",
                e.interface.title,
                pin_list(&e.interface)
            ),
            &[
                "install",
                "-D",
                "-m",
                "0644",
                &src,
                &format!("{USER_OVERLAY_DIR}/{n}.dtbo"),
            ],
        );
        if !users.contains(&n) {
            users.push(n);
        }
        match e.interface.kind.as_str() {
            "spi" => p.notices.push(format!(
                "{}: /dev/spidev devices appear after the reboot (the spidev module loads for the rockchip,spidev nodes).",
                e.interface.title
            )),
            "w1" => p.notices.push(
                "1-Wire needs a pull-up resistor (about 4.7 kOhm) from the data pin to 3.3 V; devices appear in /sys/bus/w1/devices."
                    .into(),
            ),
            _ => {}
        }
    }
    env.set_list("user_overlays", &users);
    if env != before {
        p.step(
            format!("Back up {ENV_FILE}"),
            Action::Backup {
                path: ENV_FILE.into(),
            },
        );
        p.step(
            format!("Enable them in {ENV_FILE}"),
            Action::WriteFile {
                path: ENV_FILE.into(),
                content: env.render(),
            },
        );
        p.reboot_required = true;
    } else {
        p.notices.push("They are already enabled.".into());
    }
    Ok(p)
}

pub fn disable_plan(root: &Root, names: &[String]) -> Result<Plan> {
    crate::uefi::refuse(root, crate::uefi::OVERLAYS)?;
    let st = status(root)?;
    let before = BootEnv::parse(&root.read(ENV_FILE)?);
    let mut env = before.clone();
    let mut users = env.list("user_overlays");
    let mut p = Plan::new(format!(
        "Disable pin header interfaces: {}",
        names.join(" ")
    ));
    let mut files = Vec::new();
    for n in names {
        let e = st
            .interfaces
            .iter()
            .find(|e| &e.interface.name == n)
            .with_context(|| format!("no interface {n}"))?;
        let ov = overlay_name(&e.interface.name);
        if !users.contains(&ov) {
            bail!("{n} is not enabled");
        }
        users.retain(|u| *u != ov);
        files.push(format!("{USER_OVERLAY_DIR}/{ov}.dtbo"));
    }
    env.set_list("user_overlays", &users);
    p.step(
        format!("Back up {ENV_FILE}"),
        Action::Backup {
            path: ENV_FILE.into(),
        },
    );
    p.step(
        format!("Disable them in {ENV_FILE}"),
        Action::WriteFile {
            path: ENV_FILE.into(),
            content: env.render(),
        },
    );
    let mut rm = vec!["rm", "-f"];
    rm.extend(files.iter().map(String::as_str));
    p.run("Delete their overlays", &rm);
    p.reboot_required = true;
    Ok(p)
}
