//! Core of hwtool: every read of the board's configuration and every
//! privileged change goes through this library.
//!
//! All paths are resolved against a [`Root`], so the same code runs against
//! the live system (`/`) and against a fake root filesystem in tests.
//! Changes are never made directly: each operation builds a [`plan::Plan`],
//! which can be printed (`--dry-run`) or executed.
//!
//! The library wraps the mechanisms Armbian's packages ship on the board:
//! `/boot/armbianEnv.txt` read by `boot.scr`, the `linux-image-*`,
//! `linux-dtb-*` and `linux-u-boot-*` packages, and the U-Boot package's
//! `/usr/lib/u-boot/platform_install.sh`. See `docs/how-it-works.md`.

pub mod board;
pub mod bootloader;
pub mod dpkg;
pub mod env;
pub mod exec;
pub mod kernel;
pub mod migrate;
pub mod overlays;
pub mod plan;
pub mod root;
pub mod storage;

pub use root::Root;

/// Where hwtool keeps its own state on the target system.
pub const STATE_DIR: &str = "/var/lib/roxora-hwtool";
/// Where boot configuration files are copied before they are changed.
pub const BACKUP_DIR: &str = "/var/backups/roxora-hwtool";
/// The boot environment file read by `boot.scr`.
pub const ENV_FILE: &str = "/boot/armbianEnv.txt";
