#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Write the pin header interface overlays and their index.

Facts and where they come from:

- Which RK3576 GPIO is on which header pin: Radxa's ROCK 4D page
  https://docs.radxa.com/en/rock4/rock4d/hardware-use/pin-gpio and ArmSoM's
  CM5-IO table in github.com/ArmSoM/documentation
  docs/main/ArmSoM Product/cm5.md at 90a836e7d7da.
- Which function a pin group carries, the order of its pins and the
  controller node: the boards' DTBs from the Roxora images (kernel
  7.3.4-edge-rockchip64), pinctrl group nodes and __symbols__. tests/interfaces/check.py
  checks every entry below against those DTBs and applies every overlay
  with fdtoverlay.

Usage: tools/gen-interfaces.py   (writes data/interfaces/<board>/)
"""

import json
import os

TOP = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# kind, name, title, node label, pinctrl groups, {pin: signal}
ROCK4D = [
    ("i2c", "i2c5-m1", "I2C5 (M1)", "i2c5", ["i2c5m1_xfer"], {36: "SCL", 32: "SDA"}),
    ("i2c", "i2c8-m1", "I2C8 (M1)", "i2c8", ["i2c8m1_xfer"], {5: "SCL", 3: "SDA"}),
    ("i2c", "i2c8-m2", "I2C8 (M2)", "i2c8", ["i2c8m2_xfer"], {16: "SCL", 18: "SDA"}),
    ("i2c", "i2c9-m1", "I2C9 (M1)", "i2c9", ["i2c9m1_xfer"], {19: "SCL", 23: "SDA"}),
    ("uart", "uart2-m0", "UART2 (M0)", "uart2", ["uart2m0_xfer"], {3: "RX", 5: "TX"}),
    ("uart", "uart3-m2", "UART3 (M2)", "uart3", ["uart3m2_xfer"], {31: "RX", 26: "TX"}),
    ("uart", "uart4-m1", "UART4 (M1)", "uart4", ["uart4m1_xfer"], {15: "RX", 11: "TX"}),
    ("uart", "uart7-m0", "UART7 (M0)", "uart7", ["uart7m0_xfer"], {18: "RX", 16: "TX"}),
    ("uart", "uart10-m1", "UART10 (M1)", "uart10", ["uart10m1_xfer"], {12: "RX", 40: "TX"}),
    ("spi", "spi1-m0", "SPI1 (M0) with spidev on CS0 and CS1", "spi1",
     ["spi1m0_csn0", "spi1m0_csn1", "spi1m0_pins"],
     {23: "CLK", 21: "MISO", 19: "MOSI", 24: "CS0", 26: "CS1"}),
    ("spi", "spi2-m1", "SPI2 (M1) with spidev on CS0 and CS1", "spi2",
     ["spi2m1_csn0", "spi2m1_csn1", "spi2m1_pins"],
     {32: "CLK", 15: "MISO", 11: "MOSI", 7: "CS0", 33: "CS1"}),
    ("pwm", "pwm0-ch0-m1", "PWM0 channel 0 (M1)", "pwm0_2ch_0", ["pwm0m1_ch0"], {26: "PWM"}),
    ("pwm", "pwm1-ch0-m1", "PWM1 channel 0 (M1)", "pwm1_6ch_0", ["pwm1m1_ch0"], {23: "PWM"}),
    ("pwm", "pwm1-ch0-m2", "PWM1 channel 0 (M2)", "pwm1_6ch_0", ["pwm1m2_ch0"], {13: "PWM"}),
    ("pwm", "pwm1-ch1-m1", "PWM1 channel 1 (M1)", "pwm1_6ch_1", ["pwm1m1_ch1"], {19: "PWM"}),
    ("pwm", "pwm1-ch2-m1", "PWM1 channel 2 (M1)", "pwm1_6ch_2", ["pwm1m1_ch2"], {33: "PWM"}),
    ("pwm", "pwm1-ch3-m1", "PWM1 channel 3 (M1)", "pwm1_6ch_3", ["pwm1m1_ch3"], {35: "PWM"}),
    ("pwm", "pwm1-ch4-m1", "PWM1 channel 4 (M1)", "pwm1_6ch_4", ["pwm1m1_ch4"], {38: "PWM"}),
    ("pwm", "pwm2-ch7-m2", "PWM2 channel 7 (M2)", "pwm2_8ch_7", ["pwm2m2_ch7"], {22: "PWM"}),
    ("w1", "w1-gpio-pin7", "1-Wire on pin 7 (GPIO1_C3)", "gpio1", [], {7: "DQ"}),
]
# Header pins the DTB already uses: the console UART and the I2C6 bus of
# the on-board EEPROM (i2c6 with i2c6m3_xfer, eeprom@50).
ROCK4D_BUSY = {8: "console UART0 TX", 10: "console UART0 RX",
               27: "I2C6 for the board EEPROM", 28: "I2C6 for the board EEPROM"}
ROCK4D_GPIO = {3: "GPIO1_C7", 5: "GPIO1_C6", 7: "GPIO1_C3", 8: "GPIO0_D4", 10: "GPIO0_D5",
               11: "GPIO1_C4", 12: "GPIO1_D1", 13: "GPIO2_C0", 15: "GPIO1_C5", 16: "GPIO2_B6",
               18: "GPIO2_B7", 19: "GPIO1_B5", 21: "GPIO1_B6", 22: "GPIO2_D7", 23: "GPIO1_B4",
               24: "GPIO1_B7", 26: "GPIO1_C0", 27: "GPIO4_C7", 28: "GPIO4_C6", 29: "GPIO3_A2",
               31: "GPIO1_C1", 32: "GPIO1_D5", 33: "GPIO1_C2", 35: "GPIO1_D2", 36: "GPIO1_D4",
               37: "GPIO3_A3", 38: "GPIO1_D3", 40: "GPIO1_D0"}

CM5IO = [
    ("i2c", "i2c3-m0", "I2C3 (M0)", "i2c3", ["i2c3m0_xfer"], {5: "SCL", 3: "SDA"}),
    ("i2c", "i2c4-m3", "I2C4 (M3)", "i2c4", ["i2c4m3_xfer"], {29: "SCL", 27: "SDA"}),
    ("i2c", "i2c7-m1", "I2C7 (M1)", "i2c7", ["i2c7m1_xfer"], {23: "SCL", 19: "SDA"}),
    ("uart", "uart2-m1", "UART2 (M1)", "uart2", ["uart2m1_xfer"], {3: "RX", 5: "TX"}),
    ("uart", "uart2-m2", "UART2 (M2)", "uart2", ["uart2m2_xfer"], {27: "RX", 29: "TX"}),
    ("uart", "uart3-m0", "UART3 (M0)", "uart3", ["uart3m0_xfer"], {19: "RX", 23: "TX"}),
    ("spi", "spi3-m0", "SPI3 (M0) with spidev on CS0", "spi3",
     ["spi3m0_csn0", "spi3m0_pins"], {23: "CLK", 21: "MISO", 19: "MOSI", 24: "CS0"}),
    ("pwm", "pwm0-ch0-m0", "PWM0 channel 0 (M0)", "pwm0_2ch_0", ["pwm0m0_ch0"], {7: "PWM"}),
    ("pwm", "pwm2-ch6-m3", "PWM2 channel 6 (M3)", "pwm2_8ch_6", ["pwm2m3_ch6"], {31: "PWM"}),
    ("w1", "w1-gpio-pin7", "1-Wire on pin 7 (GPIO0_C4)", "gpio0", [], {7: "DQ"}),
]
# The console UART, and the two LEDs of the carrier (gpio-leds led-0 and
# led-1 use GPIO2_D0 and GPIO2_D1). Pin 28 shares the fan control signal
# (ArmSoM's note under the table); nothing is offered on it.
CM5IO_BUSY = {8: "console UART0 TX", 10: "console UART0 RX",
              16: "red LED (led-1)", 18: "green LED (led-0)", 28: "fan control"}
CM5IO_GPIO = {3: "GPIO4_B4", 5: "GPIO4_B5", 7: "GPIO0_C4", 8: "GPIO0_D4", 10: "GPIO0_D5",
              16: "GPIO2_D1", 18: "GPIO2_D0", 19: "GPIO3_A1", 21: "GPIO3_A2", 23: "GPIO3_A0",
              24: "GPIO3_A3", 27: "GPIO3_B7", 28: "GPIO2_D6", 29: "GPIO3_C0", 31: "GPIO3_D6"}

BOARDS = {
    "radxa-rock-4d": ("radxa,rock-4d", ROCK4D, ROCK4D_BUSY, ROCK4D_GPIO),
    "armsom-cm5-io": ("armsom,cm5-io", CM5IO, CM5IO_BUSY, CM5IO_GPIO),
}


def gpio_number(name):
    bank = int(name[4])
    return bank, (ord(name[6]) - ord("A")) * 8 + int(name[7])


def overlay(compat, kind, label, groups, pins, gpios):
    head = f'/dts-v1/;\n/plugin/;\n\n/ {{\n\tcompatible = "{compat}", "rockchip,rk3576";\n}};\n\n'
    refs = " ".join(f"&{g}" for g in groups)
    if kind == "w1":
        (pin,) = pins
        _, n = gpio_number(gpios[pin])
        return head + (
            "&{/} {\n\tonewire {\n\t\tcompatible = \"w1-gpio\";\n"
            f"\t\tgpios = <&{label} {n} 0>;\n\t}};\n}};\n"
        )
    body = f'&{label} {{\n\tpinctrl-names = "default";\n\tpinctrl-0 = <{refs}>;\n\tstatus = "okay";\n'
    if kind == "spi":
        for cs in sorted(v for v in pins.values() if v.startswith("CS")):
            i = int(cs[2:])
            body += (
                f"\n\tspidev@{i} {{\n\t\tcompatible = \"armbian,spi-dev\", \"rockchip,spidev\";\n"
                f"\t\treg = <{i}>;\n\t\tspi-max-frequency = <10000000>;\n\t}};\n"
            )
    return head + body + "};\n"


for board, (compat, funcs, busy, gpios) in BOARDS.items():
    out = os.path.join(TOP, "data", "interfaces", board)
    os.makedirs(out, exist_ok=True)
    index = {"board": board, "busy": {str(k): v for k, v in busy.items()},
             "gpio": {str(k): v for k, v in gpios.items()}, "interfaces": []}
    for kind, name, title, label, groups, pins in funcs:
        if set(pins) & set(busy):
            raise SystemExit(f"{board} {name} uses a busy pin")
        src = overlay(compat, kind, label, groups, pins, gpios)
        with open(os.path.join(out, f"{name}.dtso"), "w") as f:
            f.write("// SPDX-License-Identifier: GPL-2.0-only\n")
            f.write(f"// {title}. Written by tools/gen-interfaces.py.\n")
            f.write(src)
        index["interfaces"].append({
            "name": name, "kind": kind, "title": title, "node": label, "groups": groups,
            "pins": [{"pin": p, "signal": s, "gpio": gpios[p]} for p, s in sorted(pins.items())],
        })
    with open(os.path.join(out, "index.json"), "w") as f:
        json.dump(index, f, indent=2)
        f.write("\n")
    print(board, len(funcs), "interfaces")
