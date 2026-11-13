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
use hwtool_core::{
    bootenv, bootinfo, cpu, fan, interfaces, kernel, leds, migrate, npu, overlays, swap, thermal,
    uefi, wifi, Root,
};

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
    /// The NPU and charsiu, the LLM runtime for it.
    #[command(subcommand)]
    Npu(NpuCmd),
    /// Boot options in /boot/armbianEnv.txt.
    #[command(subcommand)]
    Bootenv(BootenvCmd),
    /// CPU frequency per cluster.
    #[command(subcommand)]
    Cpu(CpuCmd),
    /// I2C, SPI, UART, PWM and 1-Wire on the 40-pin header.
    #[command(subcommand)]
    Interfaces(InterfacesCmd),
    /// The board's LEDs and their triggers.
    #[command(subcommand)]
    Leds(LedsCmd),
    /// Temperatures, trip points, cooling devices and fans.
    #[command(subcommand)]
    Fan(FanCmd),
    /// Wi-Fi settings.
    #[command(subcommand)]
    Wifi(WifiCmd),
    /// The swap file (/swap.img, made by cloud-init at the first boot).
    #[command(subcommand)]
    Swap(SwapCmd),
    /// UEFI images: the firmware and where the device tree came from.
    #[command(subcommand)]
    Uefi(UefiCmd),
    /// Apply hwtool's saved CPU and LED settings (run at boot by hwtool-boot.service).
    #[command(hide = true)]
    Apply,
}

#[derive(Subcommand)]
enum InterfacesCmd {
    /// List the header interfaces, their pins, state and conflicts.
    List,
    /// Enable interfaces (takes effect after a reboot).
    Enable {
        #[arg(required = true)]
        names: Vec<String>,
        #[command(flatten)]
        change: Change,
    },
    /// Disable interfaces (takes effect after a reboot).
    Disable {
        #[arg(required = true)]
        names: Vec<String>,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum LedsCmd {
    /// List the LEDs, their trigger and the triggers they offer.
    List,
    /// Set an LED's trigger, now and at every boot.
    Set {
        name: String,
        trigger: String,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum FanCmd {
    /// Show thermal zones, trip points, cooling devices and the fan.
    Show,
    /// Let the kernel drive a PWM fan from the SoC temperature (after a reboot).
    Enable {
        /// `fan` for the board's fan connector (the default), or a header
        /// PWM function from `hwtool interfaces list`.
        #[arg(long)]
        pwm: Option<String>,
        /// TEMPERATURE:PERCENT points, temperatures going up.
        #[arg(long, default_value = fan::DEFAULT_CURVE)]
        curve: String,
        #[command(flatten)]
        change: Change,
    },
    /// Remove the fan set up by `fan enable` (after a reboot).
    Disable {
        #[command(flatten)]
        change: Change,
    },
    /// Run the fan at a fixed speed (0 to 100 %) until reboot or `fan auto`.
    Set {
        percent: String,
        #[command(flatten)]
        change: Change,
    },
    /// Give the fan back to the thermal governor.
    Auto {
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum WifiCmd {
    /// Show the regulatory country, or set it (ISO 3166 code such as DE or US).
    Country {
        country: Option<String>,
        /// Also make the aic8800 driver (ROCK 4D) follow the country instead
        /// of its own permissive rules (custregd=0; not tried on hardware).
        /// Give it every time: without it the driver's own rules return.
        #[arg(long)]
        aic8800_follow_country: bool,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum UefiCmd {
    /// Show the UEFI firmware and whether the device tree is the firmware's or an ESP override.
    Status,
}

#[derive(Subcommand)]
enum SwapCmd {
    /// Show active swap, the swap file, its fstab line, memory and zram.
    Show,
    /// Make or resize the swap file, for example 2G or 512M.
    Set {
        size: String,
        /// Allow more than twice the memory, or leaving less than a quarter
        /// of the root file system free.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        change: Change,
    },
    /// Stop swapping to the file, delete it and its fstab line.
    Off {
        #[command(flatten)]
        change: Change,
    },
    /// Set vm.swappiness (0 to 200), now and at every boot.
    Swappiness {
        value: u32,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum OrderCmd {
    /// Show the boot order in U-Boot's environment.
    Show,
    /// Save a new boot order, for example: nvme mmc1 mmc0 usb.
    Set {
        #[arg(required = true)]
        targets: Vec<String>,
        #[command(flatten)]
        change: Change,
    },
    /// Erase the saved environment, so U-Boot uses its built-in order.
    Reset {
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum NpuCmd {
    /// Show the NPU driver, device tree nodes, /dev/accel and charsiu.
    Status,
    /// Install charsiu from the Armbian repository (edge kernel only).
    InstallCharsiu {
        #[command(flatten)]
        change: Change,
    },
    /// Remove charsiu (downloaded models stay).
    RemoveCharsiu {
        #[command(flatten)]
        change: Change,
    },
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
    /// Compile a .dts or .dtso overlay, install it in /boot/overlay-user and enable it.
    Add {
        file: std::path::PathBuf,
        #[command(flatten)]
        change: Change,
    },
    /// Disable a user overlay and delete it from /boot/overlay-user.
    Remove {
        name: String,
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
    /// Install or remove the kernel headers of the branch in use (DKMS needs them).
    Headers {
        #[arg(value_parser = ["install", "remove"])]
        action: String,
        #[command(flatten)]
        change: Change,
    },
    /// Hold the kernel, device tree, U-Boot and firmware packages (apt-mark hold).
    Hold {
        #[command(flatten)]
        change: Change,
    },
    /// Release the held packages.
    Unhold {
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum BootenvCmd {
    /// Show the boot options boot.scr reads and the kernel command line it builds.
    Show,
    /// Set an option, for example verbosity=7 or console=serial.
    Set {
        assignment: String,
        #[command(flatten)]
        change: Change,
    },
    /// Remove an option, so boot.scr uses its default.
    Unset {
        key: String,
        #[command(flatten)]
        change: Change,
    },
}

#[derive(Subcommand)]
enum CpuCmd {
    /// Show each CPU cluster's governor and frequency range.
    Show,
    /// Set a cluster's governor or frequency range, now and at every boot.
    Set {
        /// The cpufreq policy, for example policy0 (see `hwtool cpu show`).
        #[arg(long)]
        policy: String,
        #[arg(long)]
        governor: Option<String>,
        /// Lowest frequency in kHz.
        #[arg(long)]
        min: Option<u64>,
        /// Highest frequency in kHz.
        #[arg(long)]
        max: Option<u64>,
        #[command(flatten)]
        change: Change,
    },
    /// Remove hwtool's CPU settings (the kernel defaults return at the next boot).
    Reset {
        #[command(flatten)]
        change: Change,
    },
    /// The same as `hwtool apply` (kept for older hwtool-cpufreq.service units).
    #[command(hide = true)]
    Apply,
}

#[derive(Subcommand)]
enum BootCmd {
    /// Show the U-Boot package, disks and SPI flash.
    Status,
    /// Show the U-Boot version on SPI flash and SD/eMMC, compared with the package (needs root).
    Version,
    /// Write the package's U-Boot to the device the board starts from, if it differs.
    UpdateBootloader {
        #[command(flatten)]
        change: Change,
    },
    /// The boot order in U-Boot's environment (where it has a writable one).
    #[command(subcommand)]
    Order(OrderCmd),
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
            let v = bootinfo::versions(&root).ok();
            let u = uefi::is_uefi(&root).then(|| uefi::status(&root));
            if cli.json {
                print_json(&serde_json::json!({
                    "kernel": k.as_ref().ok(),
                    "kernel_error": k.as_ref().err().map(|e| format!("{e:#}")),
                    "boot": b,
                    "bootloader_versions": v,
                    "uefi": u,
                }))?;
            } else {
                match &k {
                    Ok(k) => print_kernel(k),
                    Err(e) => outln!("Kernel: {e:#}"),
                }
                outln!();
                if let Some(u) = &u {
                    print_uefi(u);
                    outln!();
                }
                print_boot(&b);
                if let Some(v) = &v {
                    outln!();
                    print_versions(v);
                }
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
        Command::Kernel(KernelCmd::Headers { action, change }) => {
            let p = kernel::headers_plan(&root, &mut exec, action == "install")?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Kernel(KernelCmd::Hold { change }) => {
            let p = kernel::hold_plan(&root, true)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Kernel(KernelCmd::Unhold { change }) => {
            let p = kernel::hold_plan(&root, false)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Overlays(OverlayCmd::Add { file, change }) => {
            let file = std::fs::canonicalize(file)
                .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
            let p = overlays::add_plan(&root, &mut exec, &file.display().to_string())?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Overlays(OverlayCmd::Remove { name, change }) => {
            let p = overlays::remove_plan(&root, name)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Bootenv(BootenvCmd::Show) => {
            let st = bootenv::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                for o in &st.options {
                    outln!(
                        "{:<22} {:<10} {}",
                        o.key,
                        o.value
                            .clone()
                            .or_else(|| o.default.clone().map(|d| format!("({d})")))
                            .unwrap_or_else(|| "-".into()),
                        o.help
                    );
                }
                outln!("(values in parentheses are boot.scr's defaults)");
                match &st.cmdline {
                    Some(c) => outln!("Kernel command line: {c}"),
                    None => outln!("Kernel command line: hwtool cannot follow this boot.cmd"),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Bootenv(BootenvCmd::Set { assignment, change }) => {
            let p = bootenv::set_plan(&root, assignment)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Bootenv(BootenvCmd::Unset { key, change }) => {
            let p = bootenv::unset_plan(&root, key)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Cpu(CpuCmd::Show) => {
            let st = cpu::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                for p in &st.policies {
                    outln!(
                        "{} (CPUs {}{}): {}, {} to {} MHz",
                        p.name,
                        p.cpus
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                        if p.core.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", p.core)
                        },
                        p.governor,
                        p.min_khz / 1000,
                        p.max_khz / 1000
                    );
                    outln!("  governors: {}", p.governors.join(" "));
                    outln!(
                        "  frequencies (MHz): {}",
                        p.frequencies_khz
                            .iter()
                            .map(|f| (f / 1000).to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                outln!(
                    "Applied at boot: {}",
                    if st.persistent {
                        cpu::CONF
                    } else {
                        "no (kernel defaults)"
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Cpu(CpuCmd::Set {
            policy,
            governor,
            min,
            max,
            change,
        }) => {
            let p = cpu::set_plan(&root, policy, governor.as_deref(), *min, *max)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Cpu(CpuCmd::Reset { change }) => {
            let p = cpu::reset_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Apply | Command::Cpu(CpuCmd::Apply) => {
            // Run by hwtool-boot.service: applies what the user set before.
            let p = cpu::apply_plan(&root)?;
            let change = Change {
                dry_run: false,
                yes: true,
                expect_plan: None,
            };
            apply(cli, &root, &mut exec, p, change)
        }
        Command::Npu(NpuCmd::Status) => {
            let st = npu::status(&root, &mut exec)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_npu(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Npu(NpuCmd::InstallCharsiu { change }) => {
            let p = npu::install_plan(&root, &mut exec)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Npu(NpuCmd::RemoveCharsiu { change }) => {
            let p = npu::remove_plan(&root, &mut exec)?;
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
        Command::Boot(BootCmd::Version) => {
            let v = bootinfo::versions(&root)?;
            if cli.json {
                print_json(&v)?;
            } else {
                print_versions(&v);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Boot(BootCmd::UpdateBootloader { change }) => {
            let p = bootinfo::update_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Boot(BootCmd::Order(OrderCmd::Show)) => {
            let o = bootinfo::order(&root, &mut exec)?;
            if cli.json {
                print_json(&o)?;
            } else {
                outln!("Built-in boot order: {}", o.default.join(" "));
                match (&o.writable, &o.reason) {
                    (Some(w), _) => {
                        outln!("Environment: {w}");
                        match &o.saved {
                            Some(s) => outln!("Saved boot order: {}", s.join(" ")),
                            None => outln!("Saved boot order: none (U-Boot uses the built-in one)"),
                        }
                    }
                    (None, Some(r)) => outln!("The boot order cannot be changed: {r}"),
                    _ => {}
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Boot(BootCmd::Order(OrderCmd::Set { targets, change })) => {
            let p = bootinfo::order_plan(&root, &mut exec, targets)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Boot(BootCmd::Order(OrderCmd::Reset { change })) => {
            let p = bootinfo::order_reset_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Interfaces(InterfacesCmd::List) => {
            let st = interfaces::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_interfaces(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Interfaces(InterfacesCmd::Enable { names, change }) => {
            let p = interfaces::enable_plan(&root, names)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Interfaces(InterfacesCmd::Disable { names, change }) => {
            let p = interfaces::disable_plan(&root, names)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Leds(LedsCmd::List) => {
            let st = leds::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                for l in &st.leds {
                    outln!(
                        "{:<24} {:<12} (offers: {})",
                        l.name,
                        l.trigger,
                        l.triggers.join(" ")
                    );
                }
                outln!(
                    "Kept across reboots by: {}",
                    if st.armbian_led_state {
                        "armbian-led-state.service (/etc/armbian-leds.conf)"
                    } else {
                        "hwtool-boot.service (/etc/hwtool/leds.conf)"
                    }
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Leds(LedsCmd::Set {
            name,
            trigger,
            change,
        }) => {
            let p = leds::set_plan(&root, name, trigger)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Fan(FanCmd::Show) => {
            let st = thermal::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_thermal(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Fan(FanCmd::Enable { pwm, curve, change }) => {
            let p = fan::enable_plan(&root, &mut exec, pwm.as_deref(), curve)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Fan(FanCmd::Disable { change }) => {
            let p = fan::disable_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Fan(FanCmd::Set { percent, change }) => {
            let n: u32 = percent
                .trim_end_matches('%')
                .parse()
                .map_err(|_| anyhow::anyhow!("{percent}: give a percentage, 0 to 100"))?;
            let p = fan::set_plan(&root, n)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Fan(FanCmd::Auto { change }) => {
            let p = fan::auto_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Wifi(WifiCmd::Country {
            country: None,
            aic8800_follow_country,
            ..
        }) => {
            if *aic8800_follow_country {
                bail!("--aic8800-follow-country needs a country to set");
            }
            let st = wifi::status(&root, &mut exec)?;
            if cli.json {
                print_json(&st)?;
            } else {
                outln!(
                    "Country in use: {}",
                    st.country
                        .as_deref()
                        .unwrap_or("unknown (iw is not installed)")
                );
                for (phy, c) in &st.self_managed {
                    outln!("  {phy} manages its own rules: country {c}");
                }
                outln!(
                    "Set at boot: {}",
                    st.configured
                        .as_deref()
                        .map(|c| format!("{c} ({})", wifi::MODPROBE))
                        .unwrap_or_else(|| "no".into())
                );
                if st.aic8800 {
                    outln!(
                        "aic8800 driver: {}",
                        if st.aic8800_follows_country {
                            "follows the country (custregd=0)"
                        } else {
                            "its own permissive rules (custregd on, the driver's default)"
                        }
                    );
                }
                for n in &st.notes {
                    outln!("{n}");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Wifi(WifiCmd::Country {
            country: Some(c),
            aic8800_follow_country,
            change,
        }) => {
            let p = wifi::set_plan(&root, &mut exec, c, *aic8800_follow_country)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Uefi(UefiCmd::Status) => {
            let u = uefi::status(&root);
            if cli.json {
                print_json(&u)?;
            } else {
                print_uefi(&u);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Swap(SwapCmd::Show) => {
            let st = swap::status(&root)?;
            if cli.json {
                print_json(&st)?;
            } else {
                print_swap(&st);
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Swap(SwapCmd::Set {
            size,
            force,
            change,
        }) => {
            let p = swap::set_plan(&root, size, *force)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Swap(SwapCmd::Off { change }) => {
            let p = swap::off_plan(&root)?;
            apply(cli, &root, &mut exec, p, change.clone())
        }
        Command::Swap(SwapCmd::Swappiness { value, change }) => {
            let p = swap::swappiness_plan(&root, *value)?;
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
    if st.held.is_empty() {
        outln!("Held packages: none (apt upgrades the kernel)");
    } else {
        outln!(
            "Held packages: {} (hwtool kernel unhold releases them)",
            st.held.join(", ")
        );
    }
    if !st.dkms.is_empty() {
        outln!("DKMS modules: {}", st.dkms.join(", "));
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

fn print_versions(v: &bootinfo::Versions) {
    outln!(
        "U-Boot in the package: {}",
        v.package.as_deref().unwrap_or("no version string found")
    );
    if let Some(s) = &v.package_spi {
        if Some(s) != v.package.as_ref() {
            outln!("  SPI image: {s}");
        }
    }
    for m in &v.media {
        let starts = if v.starts_from.as_deref() == Some(m.device.as_str()) {
            "  (the board starts from here)"
        } else {
            ""
        };
        outln!(
            "  {:<14} {:<4} {}{}{}",
            m.device,
            m.kind,
            m.state,
            m.version
                .as_deref()
                .map(|x| format!(": {x}"))
                .unwrap_or_default(),
            starts
        );
    }
}

fn print_interfaces(st: &interfaces::Status) {
    outln!("40-pin header interfaces for {}:", st.board);
    for e in &st.interfaces {
        let i = &e.interface;
        let pins: Vec<String> = i
            .pins
            .iter()
            .map(|p| format!("{} {}", p.pin, p.signal))
            .collect();
        outln!(
            "  [{}] {:<14} {}",
            if e.enabled { "x" } else { " " },
            i.name,
            i.title
        );
        outln!("      pins {}", pins.join(", "));
        if !e.blocked_by.is_empty() {
            outln!(
                "      cannot be enabled with {} (enabled)",
                e.blocked_by.join(", ")
            );
        } else if !e.conflicts.is_empty() {
            outln!("      not together with {}", e.conflicts.join(", "));
        }
    }
    if !st.busy.is_empty() {
        outln!("Pins the board uses itself:");
        for (pin, what) in &st.busy {
            outln!("  {pin:>2}  {what}");
        }
    }
}

fn print_uefi(u: &uefi::Status) {
    if !u.uefi {
        outln!("Not a UEFI image: this system boots with U-Boot and boot.scr.");
        return;
    }
    outln!(
        "UEFI image (GRUB), device tree from the {}",
        match (u.device_tree.as_str(), &u.override_file) {
            ("override", Some(f)) => format!("firmware's DTB override ({f})"),
            ("firmware", _) => "firmware".to_string(),
            _ => "firmware (hwtool cannot tell)".to_string(),
        }
    );
    if !u.booted_with_uefi {
        outln!("  /sys/firmware/efi is missing: this boot did not come from UEFI");
    }
    let fw: Vec<&str> = [&u.firmware_vendor, &u.firmware_version, &u.firmware_date]
        .iter()
        .filter_map(|x| x.as_deref())
        .collect();
    outln!(
        "Firmware: {}",
        if fw.is_empty() {
            "unknown (no DMI/SMBIOS information)".to_string()
        } else {
            fw.join(", ")
        }
    );
    if let Some(m) = &u.model {
        outln!("Device tree model: {m}");
    }
    outln!(
        "SoC: {} ({})",
        u.soc.as_deref().unwrap_or("unknown"),
        u.compatible.join(" ")
    );
    if let Some(s) = u.similarity {
        outln!(
            "Closest DTB on the EFI system partition: {:.1}% of properties equal",
            s * 100.0
        );
    }
    outln!(
        "DTBs for the firmware's override option: {}",
        if u.esp_dtbs.is_empty() {
            "none".to_string()
        } else {
            u.esp_dtbs.len().to_string()
        }
    );
    for n in &u.notes {
        outln!("{n}");
    }
}

fn print_swap(st: &swap::Status) {
    outln!(
        "Memory: {} ({} available)",
        human(st.mem_total_bytes),
        human(st.mem_available_bytes)
    );
    if st.active.is_empty() {
        outln!("Active swap: none");
    }
    for a in &st.active {
        outln!(
            "Active swap: {} ({}) {}, {} used, priority {}",
            a.name,
            a.kind,
            human(a.size_bytes),
            human(a.used_bytes),
            a.priority
        );
    }
    outln!(
        "Swap file: {} {}",
        st.file,
        st.file_bytes
            .map(human)
            .unwrap_or_else(|| "(does not exist)".into())
    );
    outln!(
        "fstab: {}",
        st.fstab_line.as_deref().unwrap_or("no line for it")
    );
    outln!(
        "zram: {}",
        match (st.zram, st.zram_service) {
            (true, _) => "in use (armbian-zram-config)",
            (false, true) => "not in use (armbian-zram-config is installed)",
            _ => "not in use",
        }
    );
    if let Some(s) = st.swappiness {
        outln!(
            "vm.swappiness: {s}{}",
            st.swappiness_saved
                .map(|v| format!(" ({v} set by hwtool)"))
                .unwrap_or_default()
        );
    }
}

fn print_thermal(st: &thermal::Status) {
    for z in &st.zones {
        outln!(
            "{} ({}): {}",
            z.kind,
            z.name,
            z.temp_mc
                .map(|t| format!("{:.1} C", t as f64 / 1000.0))
                .unwrap_or_else(|| "unknown".into())
        );
        for t in &z.trips {
            outln!(
                "  trip {} {:<9} {:.1} C{}",
                t.index,
                t.kind,
                t.temp_mc as f64 / 1000.0,
                if t.writable { " (writable)" } else { "" }
            );
        }
    }
    for c in &st.cooling {
        outln!(
            "Cooling {} ({}): state {} of {}",
            c.kind,
            c.name,
            c.cur_state,
            c.max_state
        );
    }
    outln!(
        "Fan: {}{}",
        if st.fan { "yes" } else { "none" },
        st.fan_pwm
            .map(|p| format!(", speed {}% (pwm1 {p} of 255)", (p * 100 + 127) / 255))
            .unwrap_or_default()
    );
    if let Some(c) = &st.configured {
        outln!(
            "Set up by hwtool fan enable: {} ({})",
            c.pwm,
            c.curve
                .iter()
                .map(|(t, d)| format!("{d}% at {t} C"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(p) = &st.policy {
        outln!("Governor of {}: {p}", fan::ZONE);
    }
    for n in &st.notes {
        outln!("{n}");
    }
}

fn print_npu(st: &npu::Status) {
    outln!("NPU: {}", st.verdict);
    outln!(
        "Kernel: {} (branch {})",
        st.kernel.as_deref().unwrap_or("unknown"),
        st.branch.as_deref().unwrap_or("unknown")
    );
    if let Some(r) = &st.running_kernel {
        outln!("  running now: {r}");
    }
    for d in [&st.rocket, &st.rknpu] {
        if d.available() || d.loaded {
            outln!(
                "Driver {}: {}{}{}",
                d.name,
                d.module_file.as_deref().unwrap_or(if d.builtin {
                    "built in"
                } else {
                    "no module"
                }),
                if d.builtin && d.module_file.is_some() {
                    ", built in"
                } else {
                    ""
                },
                if d.loaded { ", loaded" } else { ", not loaded" }
            );
        }
    }
    if !st.rocket.available() {
        outln!("Driver rocket: not in this kernel");
    }
    if st.nodes.is_empty() {
        outln!("Device tree: no NPU node");
    }
    for n in &st.nodes {
        outln!(
            "Device tree: {} {} ({})",
            n.path,
            n.compatible.join(" "),
            n.status
        );
    }
    if st.accel.is_empty() {
        outln!("Device: no /dev/accel device");
    }
    for a in &st.accel {
        outln!("Device: {} {} {}:{}", a.path, a.mode, a.owner, a.group);
    }
    outln!(
        "charsiu: {}{}",
        st.charsiu
            .installed
            .as_deref()
            .map(|v| format!("{v} installed"))
            .unwrap_or_else(|| "not installed".into()),
        match (&st.charsiu.candidate, &st.charsiu.origin) {
            (Some(c), Some(o)) => format!(", {c} available from {o}"),
            (Some(c), None) => format!(", {c} available"),
            _ => ", no package available".into(),
        }
    );
}
