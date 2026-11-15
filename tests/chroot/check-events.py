#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
#
# Check the events `hwtool ... --yes --progress-json` printed for a change
# that worked and ran apt-get: every line is an event, the plan comes
# first, each step runs and is done in order, apt reported its progress,
# and the last event is the result with the log.
#
# Usage: check-events.py <file with the standard output>
import json
import sys

events = []
with open(sys.argv[1], encoding="utf-8") as f:
    for n, line in enumerate(f, 1):
        try:
            events.append(json.loads(line))
        except ValueError:
            sys.exit(f"line {n} is not JSON: {line!r}")


def check(ok, what):
    if not ok:
        sys.exit(what)


check(events and events[0]["event"] == "plan", "the first event is not the plan")
plan = events[0]
steps = plan["steps"]
check(len(steps) >= 3, f"a plan of {len(steps)} steps")
done = events[-1]
check(done["event"] == "done", "the last event is not done")
check(done["ok"] is True, f"the run failed: {done}")
check(done["reboot"] is True and plan["reboot"] is True, "no reboot asked for")
check(done["log"].startswith("/var/log/hwtool/"), f"log: {done.get('log')}")
check(sum(e["event"] == "done" for e in events) == 1, "more than one done event")

# Steps: 1 running, 1 done, 2 running, 2 done, ...
seen = [(e["step"], e["state"], e["title"], e["of"]) for e in events if e["event"] == "step"]
want = []
for i, title in enumerate(steps, 1):
    want += [(i, "running", title, len(steps)), (i, "done", title, len(steps))]
check(seen == want, f"steps out of order:\n{seen}\nexpected:\n{want}")

# Progress belongs to the step that is running, is 0 to 100 and never goes
# back within a step.
running = None
last = {}
progress = []
for e in events:
    if e["event"] == "step":
        running = e["step"] if e["state"] == "running" else None
    elif e["event"] == "progress":
        check(e["step"] == running, f"progress outside its step: {e}")
        p = e.get("percent")
        check(p is not None and 0 <= p <= 100, f"percent: {e}")
        check(p >= last.get(e["step"], 0), f"progress went back: {e}")
        last[e["step"]] = p
        progress.append(e)
check(progress, "apt reported no progress")
titles = sorted({steps[e["step"] - 1] for e in progress})
check(any(e.get("detail") for e in progress), "progress without any detail")

lines = [e["line"] for e in events if e["event"] == "log"]
check(any("APT::Status-Fd=" in l for l in lines), "the log does not name the apt option")
check(any(l.startswith(f"[1/{len(steps)}] ") for l in lines), "no step line in the log")
print(f"{len(events)} events, {len(steps)} steps, {len(progress)} progress events in: {'; '.join(titles)}")
