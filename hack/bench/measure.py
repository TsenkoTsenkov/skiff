#!/usr/bin/env python3
"""Measures a desktop app from the outside: what a user's machine pays for it.

Nothing here is specific to Skiff. The app is started as a child in its own
session; everything it starts is found through /proc (children, and anything
still in its session) and measured as one "process tree".

  measure.py startup  --label L --mark NAME=REGEX ... [--runs 7] -- <command>
      Launches the command --runs times and times how long after launch each
      mark's regex first matches a line the app prints. With --window, also
      the time until the app has a visible X11 window (needs xdotool).

  measure.py steady   --label L [--ready-regex R | --ready-after S] -- <command>
      Launches once. Waits until the app is "ready" (a printed line matches,
      a fixed number of seconds passed, or - the default - you press Enter
      when the pod list is showing), then records: memory of the process
      tree (PSS and RSS), process and thread counts, CPU used over an idle
      window, listening sockets, and the remote addresses the tree had
      connections to in the first --net-window seconds after launch.

  measure.py trace    --label L [--seconds 60] -- <command>
      Runs the command under strace for --seconds and lists every connect(),
      bind()/listen() and exec the tree made. This is the complete record
      the socket snapshots of "steady" can miss (a connection shorter than
      the snapshot interval). Linux only; slows the app, so nothing else is
      measured in this mode.

  measure.py size     --label L PATH...
      Bytes of each file, or of everything under each directory.

  measure.py row      STEADY.json [SIZE.json]
      The Markdown table header and one row for docs/benchmarks.md, from
      saved results.

Every mode writes one JSON document (--out FILE, default standard output)
and prints a short summary on standard error. Python 3.8+, standard library
only. Linux is the tested platform. On macOS "steady" and "size" use ps,
footprint and lsof instead of /proc and ss; that path is UNTESTED.
"""

import argparse
import json
import os
import platform
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

LINUX = sys.platform.startswith("linux")
MAC = sys.platform == "darwin"


def log(msg):
    print(f"[bench] {msg}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------- launching


class App:
    """The launched command, its output (each line with the time it came)."""

    def __init__(self, cmd, env, log_path=None, echo=False):
        self.lines = []  # (seconds since launch, text)
        self.cond = threading.Condition()
        self.log = open(log_path, "w") if log_path else None
        self.echo = echo
        # The clock starts before fork/exec, so loading the program counts.
        self.t0 = time.monotonic()
        self.proc = subprocess.Popen(
            cmd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True
        )
        self.pid = self.proc.pid
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in self.proc.stdout:
            t = time.monotonic() - self.t0
            text = raw.decode("utf-8", "replace").rstrip("\n")
            if self.log:
                self.log.write(f"{t:9.4f} {text}\n")
                self.log.flush()
            if self.echo:
                print(f"    | {text}", file=sys.stderr, flush=True)
            with self.cond:
                self.lines.append((t, text))
                self.cond.notify_all()

    def elapsed(self):
        return time.monotonic() - self.t0

    def wait_line(self, regex, timeout):
        """(time, match) of the first printed line `regex` finds, or None."""
        deadline = time.monotonic() + timeout
        seen = 0
        with self.cond:
            while True:
                for t, text in self.lines[seen:]:
                    m = regex.search(text)
                    if m:
                        return t, m
                seen = len(self.lines)
                left = deadline - time.monotonic()
                if left <= 0 or self.proc.poll() is not None and seen == len(self.lines):
                    # (One more look after the process ended, then give up.)
                    if left <= 0 or not self.cond.wait(0.2):
                        if seen == len(self.lines):
                            return None
                else:
                    self.cond.wait(min(left, 0.5))

    def stop(self):
        """Ends the whole tree: politely, then not."""
        pids = tree(self.pid)
        for sig, grace in ((signal.SIGTERM, 3.0), (signal.SIGKILL, 2.0)):
            for pid in pids:
                try:
                    os.kill(pid, sig)
                except OSError:
                    pass
            end = time.monotonic() + grace
            while time.monotonic() < end:
                if self.proc.poll() is not None and not any(alive(p) for p in pids):
                    break
                time.sleep(0.05)
            if not any(alive(p) for p in pids):
                break
        try:
            self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        if self.log:
            self.log.close()


def alive(pid):
    if LINUX:
        try:
            with open(f"/proc/{pid}/stat") as f:
                # A zombie has ended; it only waits to be collected.
                return f.read().rsplit(")", 1)[1].split()[0] != "Z"
        except OSError:
            return False
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def build_env(args, state_dir=None):
    env = dict(os.environ)
    for item in args.env or []:
        key, _, value = item.partition("=")
        env[key] = value
    for key in args.unset or []:
        env.pop(key, None)
    if state_dir:
        # An app that has never been run on this account: no saved window,
        # no last-used cluster, no settings.
        for var, sub in (("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state")):
            env[var] = os.path.join(state_dir, sub)
            os.makedirs(env[var], exist_ok=True)
    return env


# ------------------------------------------------------------ process tree


def _proc_stat(pid):
    """(name, state, ppid, session, utime+stime in ticks) from /proc."""
    with open(f"/proc/{pid}/stat") as f:
        data = f.read()
    name = data[data.index("(") + 1 : data.rindex(")")]
    rest = data[data.rindex(")") + 2 :].split()
    return name, rest[0], int(rest[1]), int(rest[3]), int(rest[11]) + int(rest[12])


def tree(root):
    """The root, its descendants, and whatever else is in its session."""
    if not LINUX:
        return _tree_ps(root)
    info = {}
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                info[int(entry)] = _proc_stat(entry)
            except (OSError, ValueError):
                pass
    found = {root} if root in info else set()
    found |= {pid for pid, st in info.items() if st[3] == root}
    grew = True
    while grew:
        grew = False
        for pid, st in info.items():
            if pid not in found and st[2] in found:
                found.add(pid)
                grew = True
    return sorted(p for p in found if info[p][1] != "Z")


def _tree_ps(root):
    out = subprocess.run(["ps", "-axo", "pid=,ppid="], capture_output=True, text=True).stdout
    parent = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            parent[int(parts[0])] = int(parts[1])
    found = {root} if root in parent else set()
    grew = True
    while grew:
        grew = False
        for pid, ppid in parent.items():
            if pid not in found and ppid in found:
                found.add(pid)
                grew = True
    return sorted(found)


def _kb_fields(path, wanted):
    got = {}
    with open(path) as f:
        for line in f:
            key, _, rest = line.partition(":")
            if key in wanted:
                got[key] = int(rest.split()[0]) * 1024
    return got


def snapshot(root):
    """Memory, threads and CPU time of every process in the tree, now."""
    procs = []
    for pid in tree(root):
        try:
            procs.append(_snapshot_linux(pid) if LINUX else _snapshot_mac(pid))
        except (OSError, ValueError, subprocess.SubprocessError):
            pass  # ended while we looked
    total = {"processes": len(procs), "threads": sum(p["threads"] for p in procs)}
    for key in ("pss", "pss_anon", "pss_file", "pss_shmem", "rss", "footprint"):
        values = [p[key] for p in procs if p.get(key) is not None]
        if values:
            total[key] = sum(values)
    return {"at": time.time(), "total": total, "procs": procs}


def _snapshot_linux(pid):
    name, _, _, _, ticks = _proc_stat(pid)
    status = _kb_fields(f"/proc/{pid}/status", {"VmRSS", "Threads"})
    with open(f"/proc/{pid}/status") as f:
        threads = next(int(line.split()[1]) for line in f if line.startswith("Threads:"))
    p = {"pid": pid, "name": name, "threads": threads, "rss": status.get("VmRSS", 0), "cpu_s": ticks / os.sysconf("SC_CLK_TCK")}
    try:
        # PSS: private pages in full, shared pages divided by the number of
        # processes sharing them. Sums over a tree without double counting.
        roll = _kb_fields(f"/proc/{pid}/smaps_rollup", {"Rss", "Pss", "Pss_Anon", "Pss_File", "Pss_Shmem"})
        p.update(rss=roll.get("Rss", p["rss"]), pss=roll.get("Pss"), pss_anon=roll.get("Pss_Anon"), pss_file=roll.get("Pss_File"), pss_shmem=roll.get("Pss_Shmem"))
    except OSError:
        p["pss"] = None  # not ours to read (run as the same user, or root)
    return p


def _snapshot_mac(pid):
    # UNTESTED. rss in KB; time as [dd-]hh:mm:ss.cc of CPU used so far.
    out = subprocess.run(["ps", "-o", "rss=,time=,comm=", "-p", str(pid)], capture_output=True, text=True, check=True).stdout.split(None, 2)
    cpu = 0.0
    for part in out[1].replace("-", ":").split(":"):
        cpu = cpu * 60 + float(part)
    threads = max(len(subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1, 1)
    p = {"pid": pid, "name": os.path.basename(out[2].strip()), "threads": threads, "rss": int(out[0]) * 1024, "cpu_s": cpu}
    # phys_footprint is what Activity Monitor's "Memory" column shows.
    foot = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"phys_footprint:\s*([\d.]+)\s*(B|KB|MB|GB)", foot)
    if m:
        p["footprint"] = int(float(m.group(1)) * {"B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30}[m.group(2)])
    return p


def cpu_between(before, after):
    """CPU seconds the tree used between two snapshots. A process that ended
    in between takes its last ticks with it (short-lived helpers are slightly
    undercounted); one that started in between is counted whole."""
    old = {p["pid"]: p["cpu_s"] for p in before["procs"]}
    return sum(max(p["cpu_s"] - old.get(p["pid"], 0.0), 0.0) for p in after["procs"])


# ----------------------------------------------------------------- sockets

_SS_USER = re.compile(r'\("([^"]*)",pid=(\d+),fd=\d+\)')


def sockets(pids):
    """(listening, connected) sockets owned by `pids`, each a dict."""
    pids = set(pids)
    if shutil.which("ss"):
        return _sockets_ss(pids)
    return _sockets_lsof(pids)


def _sockets_ss(pids):
    listening, connected = [], []
    for proto, flag in (("tcp", "-t"), ("udp", "-u")):
        out = subprocess.run(["ss", "-H", "-a", "-n", "-p", flag], capture_output=True, text=True).stdout
        for line in out.splitlines():
            users = [(n, int(p)) for n, p in _SS_USER.findall(line) if int(p) in pids]
            if not users:
                continue
            cols = line.split()
            state, local, peer = cols[0], cols[3], cols[4]
            for name, pid in users:
                row = {"proto": proto, "process": name, "pid": pid, "local": local, "state": state}
                if state == "LISTEN" or (proto == "udp" and peer.rsplit(":", 1)[-1] == "*"):
                    listening.append(row)
                else:
                    connected.append(dict(row, peer=peer))
    return listening, connected


def _sockets_lsof(pids):
    # Also the macOS path (UNTESTED there).
    listening, connected = [], []
    if not pids or not shutil.which("lsof"):
        return listening, connected
    out = subprocess.run(["lsof", "-nP", "-i", "-a", "-p", ",".join(map(str, sorted(pids)))], capture_output=True, text=True).stdout
    for line in out.splitlines()[1:]:
        cols = line.split()
        if len(cols) < 9:
            continue
        proto, name = cols[7].lower(), cols[8]
        state = cols[9].strip("()") if len(cols) > 9 else ""
        row = {"proto": proto, "process": cols[0], "pid": int(cols[1]), "state": state}
        if "->" in name:
            local, peer = name.split("->", 1)
            connected.append(dict(row, local=local, peer=peer))
        else:
            listening.append(dict(row, local=name))
    return listening, connected


class NetWatch(threading.Thread):
    """Snapshots the tree's sockets a few times a second and remembers every
    peer address and every listening socket it ever saw."""

    def __init__(self, app, seconds, interval):
        super().__init__(daemon=True)
        self.app, self.seconds, self.interval = app, seconds, interval
        self.peers, self.listen, self.names, self.snapshots = {}, {}, {}, 0
        self.max_procs = 0

    def run(self):
        while self.app.elapsed() < self.seconds and self.app.proc.poll() is None:
            t = self.app.elapsed()
            pids = tree(self.app.pid)
            self.max_procs = max(self.max_procs, len(pids))
            for pid in pids:
                if LINUX and pid not in self.names:
                    try:
                        self.names[pid] = _proc_stat(pid)[0]
                    except (OSError, ValueError):
                        pass
            listening, connected = sockets(pids)
            self.snapshots += 1
            for row in connected:
                key = (row["proto"], row["process"], row["peer"])
                seen = self.peers.setdefault(key, {"proto": row["proto"], "process": row["process"], "peer": row["peer"], "first_seen_s": round(t, 2), "snapshots": 0})
                seen["snapshots"] += 1
                seen["last_seen_s"] = round(t, 2)
            for row in listening:
                key = (row["proto"], row["process"], row["local"])
                self.listen.setdefault(key, {"proto": row["proto"], "process": row["process"], "local": row["local"], "first_seen_s": round(t, 2)})
            time.sleep(self.interval)

    def result(self):
        return {
            "window_s": self.seconds,
            "interval_s": self.interval,
            "snapshots": self.snapshots,
            "peak_processes": self.max_procs,
            "process_names_seen": sorted(set(self.names.values())),
            "remote_addresses": sorted(self.peers.values(), key=lambda r: r["first_seen_s"]),
            "listening": sorted(self.listen.values(), key=lambda r: r["first_seen_s"]),
        }


# ------------------------------------------------------------------- modes


def machine():
    info = {"platform": platform.platform(), "cpus": os.cpu_count(), "python": platform.python_version()}
    if LINUX:
        try:
            info["ram_bytes"] = _kb_fields("/proc/meminfo", {"MemTotal"})["MemTotal"]
            with open("/proc/cpuinfo") as f:
                info["cpu_model"] = next((line.split(":", 1)[1].strip() for line in f if line.startswith("model name")), None)
        except (OSError, KeyError):
            pass
    return info


def spread(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return {"n": len(values), "median": statistics.median(values), "min": min(values), "max": max(values)}


_DURATION = re.compile(r"([\d.]+)\s*(ns|µs|us|ms|s)\b")


def parse_duration(text):
    """Seconds from "83.2ms", "1.04s" (how Rust and Go print durations)."""
    m = _DURATION.search(text or "")
    return float(m.group(1)) * {"ns": 1e-9, "µs": 1e-6, "us": 1e-6, "ms": 1e-3, "s": 1.0}[m.group(2)] if m else None


def drop_caches():
    """Forgets cached file pages, so the next launch reads the program from
    disk as after a reboot. Needs root; says so when it cannot."""
    try:
        os.sync()
        with open("/proc/sys/vm/drop_caches", "w") as f:
            f.write("3\n")
        return True
    except OSError:
        return False


def window_time(app, timeout):
    """Seconds after launch at which some process of the tree first has a
    visible X11 window (polled; the answer is late by up to one poll)."""
    while app.elapsed() < timeout and app.proc.poll() is None:
        for pid in tree(app.pid):
            out = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(pid)], capture_output=True, text=True)
            if out.stdout.strip():
                return app.elapsed()
        time.sleep(0.005)
    return None


def mode_startup(args, cmd):
    marks = []
    for item in args.mark or []:
        name, _, pattern = item.partition("=")
        marks.append((name, re.compile(pattern)))
    if not marks and not args.window:
        sys.exit("startup: give at least one --mark NAME=REGEX, or --window")
    if args.window and not shutil.which("xdotool"):
        sys.exit("startup --window needs xdotool")
    runs = []
    for i in range(args.runs):
        cold = False
        if args.drop_caches:
            cold = drop_caches()
            if not cold and i == 0:
                log("cannot drop the page cache (not root): these are WARM starts")
        state = tempfile.mkdtemp(prefix="bench-state-") if args.fresh_state else None
        app = App(cmd, build_env(args, state), log_path=args.log and f"{args.log}.{i + 1}")
        run = {"run": i + 1, "cold": cold, "marks": {}, "self_reported": {}}
        try:
            if args.window:
                run["window_s"] = window_time(app, args.timeout)
            for name, regex in marks:
                hit = app.wait_line(regex, args.timeout - app.elapsed())
                if hit is None:
                    run["marks"][name] = None
                    log(f"run {i + 1}: '{name}' never appeared (waited {args.timeout:.0f} s)")
                    continue
                t, m = hit
                run["marks"][name] = t
                # What the app itself says, when the regex captures it.
                if "self" in m.groupdict():
                    run["self_reported"][name] = parse_duration(m.group("self"))
        finally:
            app.stop()
            if state:
                shutil.rmtree(state, ignore_errors=True)
        runs.append(run)
        log(f"run {i + 1}/{args.runs}: " + ", ".join(f"{k} {v * 1000:.0f} ms" if v is not None else f"{k} -" for k, v in {**({'window': run.get('window_s')} if args.window else {}), **run["marks"]}.items()))
        time.sleep(args.pause)
    summary = {}
    for name, _ in marks:
        summary[name] = {"external_s": spread([r["marks"][name] for r in runs]), "self_reported_s": spread([r["self_reported"].get(name) for r in runs])}
    if args.window:
        summary["window"] = {"external_s": spread([r.get("window_s") for r in runs])}
    return {"mode": "startup", "cold": all(r["cold"] for r in runs), "runs": runs, "summary": summary}


def wait_ready(args, app):
    if args.ready_regex:
        hit = app.wait_line(re.compile(args.ready_regex), args.timeout)
        if hit is None:
            app.stop()
            sys.exit(f"the app never printed a line matching {args.ready_regex!r} in {args.timeout:.0f} s")
        return hit[0], "printed line"
    if args.ready_after is not None:
        time.sleep(max(args.ready_after - app.elapsed(), 0))
        return app.elapsed(), "fixed delay"
    # The app cannot tell us, so a person does. Bring the app to the state
    # being compared (the list of all pods of the test cluster on screen,
    # fully loaded), then come back to this terminal.
    print(f"\n>>> {args.label} is starting. Open the list of ALL pods in the test cluster.\n>>> Press Enter here when the pod list is showing (fully loaded), then do not touch the app.", file=sys.stderr, flush=True)
    try:
        with open("/dev/tty") as tty:
            tty.readline()
    except OSError:
        sys.stdin.readline()
    return app.elapsed(), "manual (Enter pressed)"


def mode_steady(args, cmd):
    state = tempfile.mkdtemp(prefix="bench-state-") if args.fresh_state else None
    app = App(cmd, build_env(args, state), log_path=args.log, echo=args.echo)
    net = NetWatch(app, args.net_window, args.net_interval)
    net.start()
    try:
        ready_s, how = wait_ready(args, app)
        log(f"ready after {ready_s:.1f} s ({how}); settling {args.settle:.0f} s")
        time.sleep(args.settle)
        if app.proc.poll() is not None:
            sys.exit(f"the app exited (status {app.proc.returncode}) before it could be measured")
        loaded = snapshot(app.pid)
        log(f"measuring idle CPU for {args.idle:.0f} s - do not touch the app")
        t_a = time.monotonic()
        time.sleep(args.idle)
        after = snapshot(app.pid)
        wall = time.monotonic() - t_a
        cpu = cpu_between(loaded, after)
        listening_now, _ = sockets(tree(app.pid))
        net.join(timeout=max(args.net_window - app.elapsed(), 0) + 5)
        shot = None
        if args.screenshot and shutil.which("import"):
            # Evidence of what was on screen while the numbers were taken.
            done = subprocess.run(["import", "-window", "root", args.screenshot], capture_output=True)
            shot = args.screenshot if done.returncode == 0 else None
    finally:
        app.stop()
        if state:
            shutil.rmtree(state, ignore_errors=True)
    return {
        "mode": "steady",
        "ready_s": ready_s,
        "ready_by": how,
        "settle_s": args.settle,
        "loaded": loaded,
        "after_idle": after,
        "idle": {"wall_s": wall, "cpu_s": cpu, "percent_of_one_core": 100.0 * cpu / wall},
        "listening_at_end": listening_now,
        "network": net.result(),
        "screenshot": shot,
    }


_CONNECT = re.compile(r"^(\d+)\s+([\d.]+)\s+(connect|bind)\(\d+, \{sa_family=(\w+)(.*?)\}, \d+\)\s+=\s+(-?\d+)(.*)$")
_LISTEN = re.compile(r"^(\d+)\s+([\d.]+)\s+listen\((\d+), ")
_EXEC = re.compile(r'^(\d+)\s+([\d.]+)\s+execve\("([^"]*)", \[(.*?)\](?:, .*)?\)\s+=\s+0')


def mode_trace(args, cmd):
    if not LINUX or not shutil.which("strace"):
        sys.exit("trace needs Linux and strace")
    out_file = args.log or tempfile.mktemp(prefix="bench-strace-")
    state = tempfile.mkdtemp(prefix="bench-state-") if args.fresh_state else None
    # -s: long enough to keep a whole command line; -r would hide absolute
    # order, so times are since the epoch and made relative below.
    wrapped = ["strace", "-f", "-qq", "-ttt", "-s", "400", "-e", "trace=connect,bind,listen,execve", "-o", out_file] + cmd
    app = App(wrapped, build_env(args, state))
    try:
        time.sleep(args.seconds)
    finally:
        app.stop()
        if state:
            shutil.rmtree(state, ignore_errors=True)
    first, connects, binds, listens, execs = None, {}, {}, 0, []
    with open(out_file, errors="replace") as f:
        for line in f:
            m = _EXEC.match(line)
            if m:
                first = first or float(m.group(2))
                execs.append({"t_s": round(float(m.group(2)) - first, 3), "path": m.group(3), "argv": m.group(4)})
                continue
            if _LISTEN.match(line):
                listens += 1
                continue
            m = _CONNECT.match(line)
            if not m:
                continue
            _, t, call, family, rest, ret, tail = m.groups()
            if family == "AF_INET":
                a = re.search(r'sin_port=htons\((\d+)\), sin_addr=inet_addr\("([^"]+)"\)', rest)
                addr = f"{a.group(2)}:{a.group(1)}" if a else rest
            elif family == "AF_INET6":
                a = re.search(r'sin6_port=htons\((\d+)\).*?inet_pton\(AF_INET6, "([^"]+)"', rest)
                addr = f"[{a.group(2)}]:{a.group(1)}" if a else rest
            elif family == "AF_UNIX":
                a = re.search(r'sun_path=(@?"[^"]*")', rest)
                addr = "unix:" + (a.group(1).replace('"', "") if a else "?")
            else:
                addr = family  # netlink and the like: not a network peer
            book = connects if call == "connect" else binds
            # (A non-blocking connect "fails" with EINPROGRESS and goes on.)
            ok = ret == "0" or "EINPROGRESS" in tail
            entry = book.setdefault((family, addr), {"family": family, "address": addr, "calls": 0, "succeeded": 0, "first_s": round(float(t) - (first or float(t)), 3)})
            entry["calls"] += 1
            entry["succeeded"] += 1 if ok else 0
    return {
        "mode": "trace",
        "seconds": args.seconds,
        "strace_log": out_file,
        "connect": sorted(connects.values(), key=lambda e: e["first_s"]),
        "bind": sorted(binds.values(), key=lambda e: e["first_s"]),
        "listen_calls": listens,
        # (The first exec is the app itself.)
        "exec": execs,
    }


def path_bytes(path):
    if os.path.isdir(path) and not os.path.islink(path):
        total, seen = 0, set()
        for base, _, files in os.walk(path):
            for name in files:
                try:
                    st = os.lstat(os.path.join(base, name))
                except OSError:
                    continue
                if (st.st_dev, st.st_ino) not in seen:  # a hard link once
                    seen.add((st.st_dev, st.st_ino))
                    total += st.st_size
        return total
    return os.stat(path).st_size


def mode_size(args, paths):
    return {"mode": "size", "paths": [{"path": p, "bytes": path_bytes(p), "directory": os.path.isdir(p)} for p in paths]}


ROW_HEADER = "| App | Memory, PSS | Memory, RSS | Processes | Threads | Idle CPU, % of one core | Listening sockets | Remote addresses, first 60 s | Size on disk | Pod list judged ready by |"


def mode_row(paths):
    docs = []
    for path in paths:
        with open(path) as f:
            docs.append(json.load(f))
    steady = next((d for d in docs if d["result"]["mode"] == "steady"), None)
    size = next((d for d in docs if d["result"]["mode"] == "size"), None)
    if steady is None:
        sys.exit("row: give the .steady.json of a run")
    r = steady["result"]
    t = r["loaded"]["total"]
    # (macOS has no PSS; its phys_footprint is the nearest thing.)
    memory = mb(t.get("pss")) if t.get("pss") is not None else (mb(t.get("footprint")) + " (footprint)" if t.get("footprint") else "-")
    listening = sorted({f"{x['proto']} {x['local']} ({x['process']})" for x in r["network"]["listening"]})
    peers = sorted({f"{x['peer']} ({x['process']})" for x in r["network"]["remote_addresses"]})
    sizes = "; ".join(f"{os.path.basename(p['path'].rstrip('/'))} {p['bytes'] / 1e6:.1f} MB" for p in size["result"]["paths"]) if size else "not measured"
    cells = [
        steady["label"] + (" (macOS, untested script path)" if steady.get("untested_platform") else ""),
        memory,
        mb(t.get("rss")),
        str(t["processes"]) + (f" (peak {r['network']['peak_processes']})" if r["network"]["peak_processes"] > t["processes"] else ""),
        str(t["threads"]),
        f"{r['idle']['percent_of_one_core']:.1f}",
        "<br>".join(listening) or "none",
        "<br>".join(peers) or "none seen",
        sizes,
        r["ready_by"],
    ]
    print(ROW_HEADER)
    print("|" + " --- |" * len(cells))
    print("| " + " | ".join(cells) + " |")


# -------------------------------------------------------------------- main


def mb(n):
    return "-" if n is None else f"{n / (1 << 20):.0f} MiB"


def summarize(doc):
    r = doc["result"]
    if r["mode"] == "startup":
        for name, s in r["summary"].items():
            for kind in ("external_s", "self_reported_s"):
                v = s.get(kind)
                if v:
                    log(f"{name} ({kind[:-2]}): median {v['median'] * 1000:.0f} ms, min {v['min'] * 1000:.0f}, max {v['max'] * 1000:.0f}, n={v['n']}")
    elif r["mode"] == "steady":
        for when in ("loaded", "after_idle"):
            t = r[when]["total"]
            log(f"{when}: PSS {mb(t.get('pss'))}, RSS {mb(t.get('rss'))}, {t['processes']} process(es), {t['threads']} threads")
        log(f"idle CPU: {r['idle']['cpu_s']:.2f} s over {r['idle']['wall_s']:.0f} s = {r['idle']['percent_of_one_core']:.2f}% of one core")
        log("listening: " + (", ".join(f"{x['proto']} {x['local']} ({x['process']})" for x in r["network"]["listening"]) or "none"))
        log("remote addresses: " + (", ".join(f"{x['peer']} ({x['process']})" for x in r["network"]["remote_addresses"]) or "none"))
    elif r["mode"] == "trace":
        for e in r["connect"]:
            log(f"connect {e['address']} x{e['calls']} (first at {e['first_s']} s)")
        for e in r["bind"]:
            log(f"bind {e['address']} x{e['calls']}")
        log(f"listen() calls: {r['listen_calls']}; programs started: " + ", ".join(e["path"] for e in r["exec"]))
    elif r["mode"] == "size":
        for p in r["paths"]:
            log(f"{p['path']}: {p['bytes']} bytes ({p['bytes'] / 1e6:.1f} MB)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="mode", required=True)

    def common(p, launches=True):
        p.add_argument("--label", required=True, help="what is measured, with its version, e.g. 'Skiff 0.1.0'")
        p.add_argument("--out", help="write the JSON here instead of standard output")
        if launches:
            p.add_argument("--env", action="append", metavar="K=V", help="set this for the app (repeatable)")
            p.add_argument("--unset", action="append", metavar="K", help="remove this variable for the app (repeatable)")
            p.add_argument("--fresh-state", action="store_true", help="give the app empty XDG config/data/state directories")
            p.add_argument("--log", help="keep what the app prints (trace: the strace output) in this file")
            p.add_argument("--timeout", type=float, default=120.0, help="give up waiting after this many seconds")

    p = sub.add_parser("startup")
    common(p)
    p.add_argument("--runs", type=int, default=7)
    p.add_argument("--mark", action="append", metavar="NAME=REGEX", help="time until a printed line matches; a group named 'self' is read as the app's own figure")
    p.add_argument("--window", action="store_true", help="also time until a visible X11 window exists")
    p.add_argument("--drop-caches", action="store_true", help="drop the page cache before each launch (root)")
    p.add_argument("--pause", type=float, default=1.0, help="seconds between runs")

    p = sub.add_parser("steady")
    common(p)
    p.add_argument("--ready-regex", help="ready when the app prints a line matching this")
    p.add_argument("--ready-after", type=float, help="ready this many seconds after launch")
    p.add_argument("--settle", type=float, default=5.0, help="seconds between ready and the memory snapshot")
    p.add_argument("--idle", type=float, default=60.0, help="length of the idle CPU window")
    p.add_argument("--net-window", type=float, default=60.0, help="watch sockets for this long after launch")
    p.add_argument("--net-interval", type=float, default=0.2, help="seconds between socket snapshots")
    p.add_argument("--screenshot", help="save what the X display shows at the end (needs ImageMagick)")
    p.add_argument("--echo", action="store_true", help="show what the app prints")

    p = sub.add_parser("trace")
    common(p)
    p.add_argument("--seconds", type=float, default=60.0)

    p = sub.add_parser("size")
    common(p, launches=False)

    argv = sys.argv[1:]
    if argv[:1] == ["row"]:
        return mode_row(argv[1:])
    cmd = []
    if "--" in argv:
        cut = argv.index("--")
        argv, cmd = argv[:cut], argv[cut + 1 :]
    args, extra = ap.parse_known_args(argv)
    if args.mode == "size":
        result = mode_size(args, extra + cmd)
    else:
        if extra:
            ap.error(f"unknown arguments: {' '.join(extra)}")
        if not cmd:
            ap.error("give the launch command after --")
        if not (LINUX or MAC):
            sys.exit("only Linux and macOS are supported")
        if MAC and args.mode != "steady":
            sys.exit(f"'{args.mode}' is not implemented on macOS")
        result = {"startup": mode_startup, "steady": mode_steady, "trace": mode_trace}[args.mode](args, cmd)
    doc = {
        "label": args.label,
        "date": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "command": cmd,
        "machine": machine(),
        "untested_platform": not LINUX,
        "result": result,
    }
    summarize(doc)
    text = json.dumps(doc, indent=1)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text + "\n")
    else:
        print(text)


if __name__ == "__main__":
    main()
