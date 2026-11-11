# Armbian u-boot install script for linux-u-boot-radxa-rock-4d-edge 2026.04-S88dc-Pbe40-H5267-V577e-B3ba1-R448a
# This file provides functions for deploying u-boot to a block device.
DIR=/usr/lib/linux-u-boot-edge-radxa-rock-4d



write_uboot_platform () 
{ 
    dd "if=$1/u-boot-rockchip.bin" "of=$2" bs=32k seek=1 conv=notrunc status=none
}
write_uboot_platform_mtd () 
{ 
    flashcp -v -p "$1/u-boot-rockchip-spi.bin" /dev/mtd0
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
