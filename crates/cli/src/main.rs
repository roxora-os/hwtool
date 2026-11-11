//! hwtool: device tree overlays, kernel branch and boot storage for Roxora
//! boards. All logic is in the core library; this file parses arguments,
//! prints, and asks for confirmation.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use hwtool_core::bootloader::{self, Target};
use hwtool_core::exec::SystemExecutor;
use hwtool_core::plan::{self, Action, Plan};
use hwtool_core::{kernel, migrate, overlays, Root};

// Output never panics: when the terminal or the pipe to the GTK app is
// gone, writes fail quietly and the change runs to its end or its cleanup.
macro_rules! outln { ($($t:tt)*) => {{ use std::io::Write as _; let _ = writeln!(std::io::stdout(), $($t)*); }} }
macro_rules! outp { ($($t:tt)*) => {{ use std::io::Write as _; let _ = write!(std::io::stdout(), $($t)*); }} }
macro_rules! errln { ($($t:tt)*) => {{ use std::io::Write as _; let _ = writeln!(std::io::stderr(), $($t)*); }} }
macro_rules! errp { ($($t:tt)*) => {{ use std::io::Write as _; let _ = write!(std::io::stderr(), $($t)*); }} }

#[derive(Parser)]
#[command(
    name = "hwtool",
    version,
    about = "Device tree overlays, kernel branch and boot storage for Roxora boards",
    after_help = "Every command that changes the system shows what it will do and asks first.\n\
                  Add --dry-run to only show it."
)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Work on another root filesystem (for tests). Commands that would run
    /// programs are refused there.
    #[arg(long, global = true, hide = true, default_value = "/")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct Change {
    /// Show exactly what would be run and written, and change nothing.
    #[arg(long)]
    dry_run: bool,
    /// Do not ask for confirmation.
    #[arg(short, long)]
    yes: bool,
    /// Go ahead only if the plan has this fingerprint (from an earlier
    /// --dry-run --json). The GTK app uses it so that what runs as root is
    /// what the user confirmed.
    #[arg(long, value_name = "FINGERPRINT")]
    expect_plan: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the board, kernel branch and boot storage.
    Info,
    /// Device tree overlays applied by boot.scr.
    #[command(subcommand)]
    Overlays(OverlayCmd),
    /// Kernel branch (for example edge or vendor).
    #[command(subcommand)]
    Kernel(KernelCmd),
    /// Bootloader and system storage.
    #[command(subcommand)]
    Boot(BootCmd),
}

#[derive(Subcommand)]
enum OverlayCmd {
    /// List the overlays available for this board and which are enabled.
    List,
    /// Enable overlays (takes effect after a reboot).
    Enable {
        #[arg(required = true)]
        names: Vec<String>,
        #[command(flatten)]
        change: Change,
    },
    /// Disable overlays (takes effect after a reboot).
    Disable {
        #[arg(required = true)]
        names: Vec<String>,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum KernelCmd {
    /// Show the installed kernel branch and the branches this board supports.
    Status,
    /// Install another branch's kernel, device tree and U-Boot packages and remove the old ones.
    Switch {
        branch: String,
        /// Keep the old kernel and device tree packages installed.
        #[arg(long)]
        keep_old: bool,
        #[command(flatten)]
        change: Change,
    },
    /// Go back to the branch used before the last switch.
    Revert {
        #[arg(long)]
        keep_old: bool,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum BootCmd {
    /// Show the U-Boot package, disks and SPI flash.
    Status,
    /// Write U-Boot from the installed U-Boot package to SPI flash or an SD/eMMC device.
    WriteBootloader {
        /// `spi`, or a whole SD/eMMC device such as /dev/mmcblk0.
        #[arg(long)]
        target: String,
        /// Allow writing to the device the running system booted from.
        #[arg(long)]
        allow_booted_device: bool,
        #[command(flatten)]
        change: Change,
    },
    /// Copy the running system to an eMMC or NVMe device and make it bootable.
    Migrate {
        /// The eMMC or NVMe device to erase and copy to, such as /dev/nvme0n1.
        #[arg(long)]
        target: String,
        /// Also write U-Boot to SPI flash (for NVMe).
        #[arg(long)]
        write_spi: bool,
        #[command(flatten)]
        change: Change,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            errln!("hwtool: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    outln!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn run(cli: &Cli) -> Result<ExitCode> {
    let root = Root::new(&cli.root);
    // --root is for tests. With root rights (sudo, pkexec) it could make
    // hwtool read a boot configuration or a U-Boot writer from a directory
    // the caller controls, so it is refused there.
    if !root.is_system()
        && (is_root_user()
            || std::env::var_os("PKEXEC_UID").is_some()
            || std::env::var_os("SUDO_UID").is_some())
    {
        bail!("--root is for tests and is not accepted when hwtool runs with root rights");
    }
    let mut exec = SystemExecutor;
    match &cli.command {
        Command::Info => {
            let k = kernel::status(&root);
            let b = bootloader::status(&root);
            if cli.json {
                print_json(&serde_json::json!({
                    "kernel": k.as_ref().ok(),
                    "kernel_error": k.as_ref().err().map(|e| format!("{e:#}")),
                    "boot": b,
                }))?;
            } else {
                match &k {
                    Ok(k) => print_kernel(k),
                    Err(e) => outln!("Kernel: {e:#}"),
                }
                outln!();
                print_boot(&b);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Overlays(OverlayCmd::List) => {
            let st = overlays::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_overlays(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Overlays(OverlayCmd::Enable { names, change }) => {
            let p = overlays::enable(&root, names)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Overlays(OverlayCmd::Disable { names, change }) => {
            let p = overlays::disable(&root, names)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Kernel(KernelCmd::Status) => {
            let st = kernel::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_kernel(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Kernel(KernelCmd::Switch {
            branch,
            keep_old,
            change,
        }) => {
            let p = kernel::switch_plan(&root, &mut exec, branch, *keep_old, None)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Kernel(KernelCmd::Revert { keep_old, change }) => {
            let p = kernel::revert_plan(&root, &mut exec, *keep_old)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Boot(BootCmd::Status) => {
            let st = bootloader::status(&root);
            if cli.json {
                print_json(&st)?;
            } else {
                print_boot(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Boot(BootCmd::WriteBootloader {
            target,
            allow_booted_device,
            change,
        }) => {
            let p = bootloader::write_plan(&root, &Target::parse(target)?, *allow_booted_device)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Boot(BootCmd::Migrate {
            target,
            write_spi,
            change,
        }) => {
            let p = migrate::plan(&root, &mut exec, target, *write_spi)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
    }
}

fn is_root_user() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

/// Show the plan, ask, then execute it.
fn apply(
    cli: &Cli,
    root: &Root,
    exec: &mut SystemExecutor,
    plan: Plan,
    change: Change,
) -> Result<ExitCode> {
    if change.dry_run {
        if cli.json {
            print_json(&serde_json::json!({
                "dry_run": true,
                "plan": plan,
                "text": plan.render(),
                "fingerprint": plan.fingerprint(),
            }))?;
        } else {
            outp!("{}", plan.render());
            outln!(
                "\nDry run: nothing was changed. Plan fingerprint: {}",
                plan.fingerprint()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    if plan.is_empty() {
        outp!("{}", plan.render());
        for n in &plan.notices {
            outln!("{n}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let runs_programs = plan.steps.iter().any(|s| {
        matches!(
            s.action,
            Action::Run { .. }
                | Action::Shell { .. }
                | Action::PointAtNewRoot { .. }
                | Action::VerifyTarget { .. }
        )
    });
    if !root.is_system() && runs_programs {
        bail!("this change runs programs, which hwtool does not do with --root; use --dry-run");
    }
    if root.is_system() && !is_root_user() {
        bail!("this change needs root; run it with sudo (or add --dry-run to only see it)");
    }
    if let Some(fp) = &change.expect_plan {
        if *fp != plan.fingerprint() {
            bail!(
                "the plan changed since it was confirmed (fingerprint {} instead of {fp}); \
                 nothing was changed. Look at the new plan and confirm again",
                plan.fingerprint()
            );
        }
    }
    errp!("{}", plan.render());
    if !change.yes {
        if !io::stdin().is_terminal() {
            bail!("not asking for confirmation without a terminal; add --yes to go ahead");
        }
        errp!("\nType yes to go ahead: ");
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            errln!("Nothing was changed.");
            return Ok(ExitCode::from(3));
        }
    }
    errln!();
    plan.execute(root, exec, &plan::utc_stamp())?;
    errln!("\nDone.");
    if plan.reboot_required {
        errln!("Reboot for the change to take effect.");
    }
    Ok(ExitCode::SUCCESS)
}

fn print_overlays(st: &overlays::Status) {
    outln!(
        "Overlay prefixes (overlay_prefix in {}): {}",
        st.env_file,
        st.overlay_prefix.join(" ")
    );
    if st.overlays.is_empty() {
        outln!("The installed device tree package has no overlays for this board.");
    }
    for o in &st.overlays {
        outln!(
            "  [{}] {:<28} {} {}",
            if o.enabled { "x" } else { " " },
            o.name,
            o.file,
            if o.kind == "user" { "(user)" } else { "" }
        );
    }
    for m in &st.missing {
        outln!("  {m} is enabled but no file exists for it; boot.scr skips it.");
    }
}

fn print_kernel(st: &kernel::Status) {
    outln!(
        "Board: {} ({}, {})",
        st.board.board_name,
        st.board.board,
        st.board.soc
    );
    match (&st.current, &st.kernel) {
        (Some(b), Some(k)) => outln!("Kernel for the next boot: {k} (branch {b})"),
        (None, Some(k)) => outln!("Kernel for the next boot: {k} (not a branch hwtool knows)"),
        _ => outln!("Kernel for the next boot: unknown (/boot/Image is missing)"),
    }
    outln!("Branches this board supports:");
    for b in &st.branches {
        let mark = if st.current.as_deref() == Some(b.branch.as_str()) {
            "*"
        } else {
            " "
        };
        if b.installed.is_empty() {
            outln!(
                "  {mark} {:<8} not installed ({})",
                b.branch,
                b.packages.join(", ")
            );
        } else {
            let v: Vec<String> = b
                .installed
                .iter()
                .map(|p| format!("{} {}", p.name, p.version))
                .collect();
            outln!("  {mark} {:<8} {}", b.branch, v.join(", "));
        }
    }
    if let Some(h) = st.history.last() {
        outln!(
            "Last switch: {} from {} to {} ('hwtool kernel revert' goes back)",
            h.time,
            h.from_branch.as_deref().unwrap_or("unknown"),
            h.to_branch
        );
        if h.state == "pending" {
            outln!(
                "  That switch did not finish. hwtool rolled it back as far as it could; \
                 the log is in /var/log/hwtool."
            );
        }
        for p in &h.saved {
            outln!("  saved for the revert: {}", p.file);
        }
    }
}

fn human(bytes: u64) -> String {
    let gib = bytes as f64 / (1u64 << 30) as f64;
    if gib >= 1.0 {
        format!("{gib:.1} GiB")
    } else {
        format!("{:.1} MiB", bytes as f64 / (1u64 << 20) as f64)
    }
}

fn print_boot(st: &bootloader::Status) {
    match &st.uboot {
        Some(u) => {
            outln!(
                "U-Boot package: {} {}",
                u.package.as_deref().unwrap_or("(unknown)"),
                u.version.as_deref().unwrap_or("")
            );
            outln!("  files in {}: {}", u.dir, u.files.join(" "));
            outln!("  writers: {}", u.functions.join(" "));
        }
        None => outln!("U-Boot package: none"),
    }
    outln!("Disks:");
    for d in &st.disks {
        outln!(
            "  {:<14} {:<5} {:>10}{}",
            d.path,
            d.kind,
            human(d.size_bytes),
            if d.holds_root { "  running system" } else { "" }
        );
    }
    for m in &st.spi {
        outln!(
            "  {:<14} spi   {:>10}  {}",
            m.path,
            human(m.size_bytes),
            m.label
        );
    }
    for m in &st.missing {
        outln!("Not available: {m}");
    }
}
