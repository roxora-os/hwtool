//! Rebuild an installed package with the real dpkg-deb and read it back.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use common::*;
use hwtool_core::exec::SystemExecutor;
use hwtool_core::repack;

fn dpkg_deb(args: &[&str]) -> String {
    let o = Command::new("dpkg-deb").args(args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn rebuilds_from_the_dpkg_database() {
    let f = rock4d();
    let name = "linux-u-boot-radxa-rock-4d-edge";
    let d = "/usr/lib/linux-u-boot-edge-radxa-rock-4d";
    f.write(
        "/var/lib/dpkg/status",
        &format!(
            "Package: {name}\nStatus: install ok installed\nPriority: optional\nSection: kernel\n\
             Installed-Size: 11030\nMaintainer: Armbian Linux <info@armbian.com>\nArchitecture: arm64\n\
             Version: 26.11.0-trunk\nReplaces: armbian-u-boot\nProvides: armbian-u-boot\n\
             Conflicts: armbian-u-boot, u-boot-sunxi\nConffiles:\n /etc/test.conf 0123456789abcdef\n\
             Description: Das U-Boot for radxa-rock-4d\n version \"2026.04\"\n\n"
        ),
    );
    f.write("/etc/test.conf", "x=1\n");
    f.write(
        &format!("/var/lib/dpkg/info/{name}.postinst"),
        "#!/bin/bash\ntrue\n",
    );
    f.write(
        &format!("/var/lib/dpkg/info/{name}.md5sums"),
        "abc  usr/lib/u-boot/platform_install.sh\n",
    );
    std::os::unix::fs::symlink("u-boot-rockchip.bin", f.p(&format!("{d}/u-boot.link"))).unwrap();
    std::fs::set_permissions(
        f.p(&format!("{d}/u-boot-rockchip.bin")),
        std::fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    // Merged /usr: /lib is a symlink on the system, a directory in the package.
    f.write("/usr/lib/modules/7.3.4/kernel.ko", "ko");
    std::os::unix::fs::symlink("usr/lib", f.p("/lib")).unwrap();
    f.write(
        &format!("/var/lib/dpkg/info/{name}.list"),
        &format!(
            "/.\n/lib\n/lib/modules\n/lib/modules/7.3.4\n/lib/modules/7.3.4/kernel.ko\n/etc\n/etc/test.conf\n/usr\n/usr/lib\n{d}\n{d}/u-boot-rockchip.bin\n{d}/u-boot.link\n\
             /usr/lib/u-boot\n/usr/lib/u-boot/platform_install.sh\n{d}/gone.bin\n"
        ),
    );

    let out = repack::save(
        &f.root(),
        &mut SystemExecutor,
        name,
        "26.11.0-trunk",
        "/var/lib/hwtool/packages/T",
    )
    .unwrap();
    assert_eq!(
        out,
        format!("/var/lib/hwtool/packages/T/{name}_26.11.0-trunk_arm64.deb")
    );
    let deb = f.p(&out);
    let deb = deb.to_str().unwrap();

    let info = dpkg_deb(&["--info", deb]);
    assert!(info.contains(&format!("Package: {name}")), "{info}");
    assert!(
        info.contains("Conflicts: armbian-u-boot, u-boot-sunxi"),
        "{info}"
    );
    assert!(info.contains(" version \"2026.04\""), "{info}");
    assert!(
        !info.contains("Status:") && !info.contains("Conffiles:"),
        "{info}"
    );
    assert!(
        info.contains("postinst") && info.contains("md5sums") && info.contains("conffiles"),
        "{info}"
    );

    let list = dpkg_deb(&["--contents", deb]);
    assert!(list.contains("-rw-r----- root/root   9561600"), "{list}");
    assert!(
        list.contains("u-boot.link -> u-boot-rockchip.bin"),
        "{list}"
    );
    assert!(
        list.contains("./usr/lib/u-boot/platform_install.sh"),
        "{list}"
    );
    assert!(list.contains("./etc/test.conf"), "{list}");
    assert!(
        list.lines()
            .any(|l| l.starts_with('d') && l.ends_with(" ./lib/")),
        "{list}"
    );
    assert!(list.contains("./lib/modules/7.3.4/kernel.ko"), "{list}");
    assert!(!list.contains("gone.bin"), "{list}");
    // The staging directory is gone.
    assert_eq!(
        f.root().list_dir("/var/lib/hwtool/packages/T"),
        vec![format!("{name}_26.11.0-trunk_arm64.deb")]
    );
}
