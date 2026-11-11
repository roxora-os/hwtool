# Hardware test

The steps that check hwtool on a board are Part D of the Roxora hardware test:
https://github.com/roxora-os/build/blob/main/docs/hardware-test.md#part-d-hwtool

Notes for hwtool development:

- Every hardware step has a dry run first; compare its output with the
  plan you expect before running the step.
- Ask for the logs: each change writes `/var/log/hwtool/<time>.log`, and a
  failed change lists its cleanup results at the end.
- What CI covers instead of hardware: the fake-root tests (`cargo test`),
  including rollbacks, writer parsing and refusals, and the kernel branch
  switch with apt and dpkg in an Ubuntu 24.04 chroot
  (`tests/chroot/package-switch.sh`, run as root), including a switch whose
  package fails and is rolled back.
- Not covered without hardware: anything that boots, U-Boot writes to SD,
  eMMC and SPI flash, the migration, the pkexec and polkit prompt, and the
  U-Boot writer run as user nobody (CI runs the writer tests as a normal
  user).
