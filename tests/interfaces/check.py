#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Check the pin header interfaces against the boards' real DTBs.

For every interface in data/interfaces/<board>/index.json:
- each pin group exists in the DTB and its pins are, in order, the GPIOs
  index.json puts on those header pins with those signals;
- the controller node exists and the overlay compiles with dtc -@ and
  applies with fdtoverlay; in the result the node is enabled and uses the
  groups (for 1-Wire: the onewire node exists with the right GPIO);
- no interface uses a pin the DTB already uses (busy).

The DTBs in tests/interfaces/dtb are from the Roxora images (kernel
7.3.4-edge-rockchip64). Needs dtc and fdtoverlay (device-tree-compiler).
"""
import json, os, re, subprocess, sys, tempfile

TOP = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DTB = {"radxa-rock-4d": "rk3576-rock-4d.dtb", "armsom-cm5-io": "rk3576-armsom-cm5-io.dtb"}
# Pin order inside the groups (rk3576-pinctrl.dtsi): xfer groups are
# SCL,SDA (I2C) and RX,TX (UART); spi pins are CLK,MISO,MOSI.
ORDER = {"i2c": ["SCL", "SDA"], "uart": ["RX", "TX"], "spi": ["CLK", "MISO", "MOSI"]}
errors = []


def dts_of(dtb):
    return subprocess.run(["dtc", "-q", "-I", "dtb", "-O", "dts", dtb], check=True,
                          capture_output=True, text=True).stdout


def gpio(bank, pin):
    return f"GPIO{bank}_{'ABCD'[pin // 8]}{pin % 8}"


def groups_of(dts):
    out = {}
    for m in re.finditer(r"\n\t+([\w-]+) \{\n\t+rockchip,pins = <([^>]*)>;\n\t+phandle = <(0x[0-9a-f]+)>;", dts):
        v = [int(x, 16) for x in m.group(2).split()]
        out[m.group(1).replace("-", "_")] = (int(m.group(3), 16), [gpio(v[i], v[i + 1]) for i in range(0, len(v), 4)])
    return out


def symbols(dts):
    s = dts[dts.index("__symbols__"):]
    return dict(re.findall(r"\n\t\t(\w+) = \"([^\"]+)\";", s))


def node(dts, path):
    name = path.rsplit("/", 1)[-1]
    depth = path.count("/")
    m = re.search("\n" + "\t" * depth + re.escape(name) + r" \{(.*?)\n" + "\t" * depth + "\\};", dts, re.S)
    return m.group(1) if m else None


for board, dtbname in DTB.items():
    base = os.path.join(TOP, "tests", "interfaces", "dtb", dtbname)
    d = os.path.join(TOP, "data", "interfaces", board)
    idx = json.load(open(os.path.join(d, "index.json")))
    dts = dts_of(base)
    groups, syms = groups_of(dts), symbols(dts)
    busy = {int(k) for k in idx["busy"]}
    for it in idx["interfaces"]:
        name, kind = it["name"], it["kind"]
        pins = {p["signal"]: p["gpio"] for p in it["pins"]}
        if busy & {p["pin"] for p in it["pins"]}:
            errors.append(f"{board} {name}: uses a busy pin")
        if it["node"] not in syms:
            errors.append(f"{board} {name}: no node {it['node']} in the DTB")
        for g in it["groups"]:
            if g not in groups:
                errors.append(f"{board} {name}: no pin group {g}")
                continue
            got = groups[g][1]
            if g.endswith(("_xfer", "_pins")):
                want = [pins[s] for s in ORDER[kind]]
            elif "_csn" in g:
                want = [pins["CS" + g.rsplit("csn", 1)[1]]]
            else:  # pwm channel
                want = [pins["PWM"]]
            if got != want:
                errors.append(f"{board} {name}: {g} has {got}, index says {want}")
        with tempfile.TemporaryDirectory() as t:
            dtbo, merged = os.path.join(t, "o.dtbo"), os.path.join(t, "m.dtb")
            r = subprocess.run(["dtc", "-@", "-q", "-I", "dts", "-O", "dtb", "-o", dtbo,
                                os.path.join(d, name + ".dtso")], capture_output=True, text=True)
            if r.returncode:
                errors.append(f"{board} {name}: dtc: {r.stderr.strip()}")
                continue
            r = subprocess.run(["fdtoverlay", "-i", base, "-o", merged, dtbo], capture_output=True, text=True)
            if r.returncode:
                errors.append(f"{board} {name}: fdtoverlay: {r.stderr.strip()}")
                continue
            out = dts_of(merged)
            if kind == "w1":
                ow = node(out, "/onewire")
                if not ow or 'compatible = "w1-gpio"' not in ow:
                    errors.append(f"{board} {name}: no onewire node after the overlay")
                else:
                    print(f"ok {board} {name}")
                continue
            body = node(out, syms[it["node"]])
            if not body or 'status = "okay"' not in body:
                errors.append(f"{board} {name}: {it['node']} not enabled after the overlay")
                continue
            pc = re.search(r"pinctrl-0 = <([^>]*)>", body)
            want = [hex(groups[g][0]) for g in it["groups"]]
            if not pc or [hex(int(x, 16)) for x in pc.group(1).split()] != want:
                errors.append(f"{board} {name}: pinctrl-0 is {pc.group(1) if pc else None}, want {want}")
            if kind == "spi" and "armbian,spi-dev" not in body:
                errors.append(f"{board} {name}: no spidev child")
        print(f"ok {board} {name}")

if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
