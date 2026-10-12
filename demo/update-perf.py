"""Synthetic PTY probe; run from the repo root after cargo build --release."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import resource
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time

BINARY = Path.cwd() / "target/release/herdr-switchboard"
base = Path(tempfile.mkdtemp(prefix="swb-update-perf-")).resolve()
root = base / "plugin"
tools = root / "tools"
tools.mkdir(parents=True)
(root / "bin").mkdir()
(base / "home").mkdir()
(base / "config").mkdir()
(base / "config/config.toml").write_text("[common]\nnotifications = false\n")
metadata = {"result": {"plugins": [{
    "plugin_id": "switchboard", "plugin_root": str(root), "version": "9.9.9",
    "source": {"kind": "github", "owner": "crafts69guy",
               "repo": "herdr-switchboard", "managed_path": str(root)}
}]}}
(base / "metadata.json").write_text(json.dumps(metadata))
scripts = {
    tools / "herdr": '#!/bin/bash\nset -eu\nif [[ "$2" == list ]]; then cat "$PERF_METADATA"; else touch "$PERF_INSTALL"; sleep 1.4; fi\n',
    tools / "git": '#!/bin/bash\nsleep 0.2\nprintf "abcdef\\trefs/tags/v9.9.9\\n"\n',
    root / "bin/picker.sh": '#!/bin/bash\nsleep 0.8\nprintf "herdr-switchboard 9.9.9\\n"\n',
}
for path, script in scripts.items():
    path.write_text(script)
    path.chmod(0o755)
environment = {
    "HOME": str(base / "home"), "XDG_STATE_HOME": str(base / "state"),
    "HERDR_PLUGIN_CONFIG_DIR": str(base / "config"), "HERDR_PLUGIN_ROOT": str(root),
    "HERDR_BIN_PATH": str(tools / "herdr"), "PATH": str(tools) + ":/usr/bin:/bin",
    "PERF_METADATA": str(base / "metadata.json"), "PERF_INSTALL": str(base / "install.started"),
    "TERM": "xterm-256color", "LANG": "en_US.UTF-8",
}
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
before = resource.getrusage(resource.RUSAGE_CHILDREN)
start = time.monotonic()
child = subprocess.Popen([str(BINARY), "--update"], stdin=slave, stdout=slave,
                         stderr=slave, env=environment, cwd=root)
os.close(slave)
output = b""
first_frame = None
resize_sent = None
resize_latency = None
resize_offset = None
esc_sent = False
result_at = None
idle_bytes = None
idle_start = None
deadline = start + 12
try:
    while time.monotonic() < deadline:
        if (base / "install.started").exists() and not esc_sent:
            os.write(master, b"\x1b")
            esc_sent = True
            resize_sent = time.monotonic()
            resize_offset = len(output)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 40, 0, 0))
            os.kill(child.pid, __import__("signal").SIGWINCH)
        ready = select.select([master], [], [], 0.01)[0]
        if ready:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            output += chunk
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)
            if first_frame is None and b"Check installation and latest release" in plain:
                first_frame = time.monotonic() - start
            if resize_sent is not None and resize_latency is None and b"\x1b[2J" in output[resize_offset:]:
                resize_latency = time.monotonic() - resize_sent
            if b"Updated to v9.9.9." in plain and result_at is None:
                result_at = time.monotonic()
                idle_start = time.monotonic()
                idle_bytes = len(output)
        if result_at is not None and time.monotonic() - result_at > 1:
            idle_delta = len(output) - idle_bytes
            os.write(master, b"\r")
            break
        if child.poll() is not None:
            raise AssertionError("Updater closed before its result was dismissed")
    else:
        raise AssertionError("Updater did not reach its result")
    assert child.wait(timeout=3) == 0
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    elapsed = time.monotonic() - start
    assert first_frame is not None
    assert esc_sent and result_at is not None and resize_latency is not None
    print(json.dumps({
        "first_frame_ms": round(first_frame * 1000, 1),
        "resize_response_ms": round(resize_latency * 1000, 1),
        "wall_seconds_including_one_second_idle": round(elapsed, 3),
        "cpu_seconds_including_subprocesses": round(
            after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 4),
        "idle_terminal_bytes_over_one_second": idle_delta,
        "busy_escape_did_not_close": True,
        "exit_code": child.returncode,
    }, indent=2))
finally:
    if child.poll() is None:
        child.kill()
        child.wait()
    os.close(master)
    shutil.rmtree(base)
