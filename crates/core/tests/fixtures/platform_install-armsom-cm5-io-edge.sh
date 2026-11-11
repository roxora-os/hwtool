# Armbian u-boot install script for linux-u-boot-armsom-cm5-io-edge 2026.10-S5508-P9c7c-H6ced-Vc6ae-B3ba1-R448a
# This file provides functions for deploying u-boot to a block device.
DIR=/usr/lib/linux-u-boot-edge-armsom-cm5-io



write_uboot_platform () 
{ 
    dd "if=$1/u-boot-rockchip.bin" "of=$2" bs=32k seek=1 conv=notrunc status=none
}

write_uboot_platform_ufs () 
{ 
    local logging_prelude="";
    [[ $(type -t run_host_command_logged) == function ]] && logging_prelude="run_host_command_logged";
    if [[ -f $1/idbloader.img && -f $1/u-boot.itb ]]; then
        ${logging_prelude} dd if=$1/idbloader.img of=$2 bs=4096 seek=8 conv=notrunc,fsync status=none;
        ${logging_prelude} dd if=$1/u-boot.itb of=$3 bs=4096 seek=2048 conv=notrunc,fsync status=none;
    else
        echo "write_uboot_platform_ufs: no idbloader.img + u-boot.itb pair in $1; board hook must override.";
        exit 1;
    fi
}
setup_write_uboot_platform () 
{ 
    if grep -q "ubootpart" /proc/cmdline; then
        local tmp part dev;
        tmp=$(cat /proc/cmdline);
        tmp="${tmp##*ubootpart=}";
        tmp="${tmp%% *}";
        [[ -n $tmp ]] && part=$(findfs PARTUUID=$tmp 2> /dev/null);
        [[ -n $part ]] && dev=$(lsblk -n -o PKNAME $part 2> /dev/null);
        [[ -n $dev ]] && DEVICE="/dev/$dev";
    fi
}
