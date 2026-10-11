#!/usr/bin/env python3
"""Seed invented task records only inside the disposable demo home."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

home = Path.home().resolve()
if not str(home).startswith("/private/tmp/") and not str(home).startswith("/tmp/"):
    raise SystemExit("task fixtures require a disposable /tmp home")
socket = Path(os.environ["HERDR_SOCKET_PATH"])
meta = socket.stat()
stamp = f"{meta.st_dev}:{meta.st_ino}:{meta.st_ctime_ns // 10**9}:{meta.st_ctime_ns % 10**9}"
namespace = hashlib.sha256(os.fsencode(socket)).hexdigest()
agents = json.loads(subprocess.check_output(["herdr", "agent", "list"]))["result"]["agents"]
assert agents, "stage must create stand-in agents first"
tasks = []
for index, (goal, status) in enumerate([
    ("Review retry handling", "completed"),
    ("Continue the search empty state", "released"),
    ("Add keyboard navigation tests", "active"),
], 1):
    agent = agents[(index - 1) % len(agents)]
    name = f"demo-worker-{index}"
    subprocess.check_call(["herdr", "agent", "rename", agent["pane_id"], name], stdout=subprocess.DEVNULL)
    cwd = agent.get("foreground_cwd") or agent["cwd"]
    head = subprocess.check_output(["git", "-C", cwd, "rev-parse", "HEAD"], text=True).strip()
    tasks.append({
        "id": f"t{index:08x}", "generation": 1, "status": status,
        "request": {"goal": goal, "repo": cwd, "base": "HEAD", "scope": "Renderer and regression tests",
                    "acceptance": "Narrow and wide layouts pass", "constraints": "Keep the public API",
                    "references": ["README.md"], "decisions": "Reuse existing styles", "tried": "Baseline tests",
                    "open_questions": "", "expected_result": "Summary and test evidence"},
        "source_pane": os.environ["HERDR_PANE_ID"], "base_commit": head, "source_dirty": False,
        "branch": f"swb-demo-{index}", "worktree": cwd, "workspace": agent["workspace_id"],
        "target": {"pane": agent["pane_id"], "name": name, "kind": agent["agent"],
                   "native_session": agent.get("agent_session")},
        "server_stamp": stamp, "checkpoints": [], "report": None, "evidence": None,
        "step": "await_report", "error_code": None, "updated_at": int(time.time()),
    })
tasks[0]["report"] = {"status": "completed", "summary": "Retry bounds checked; no regression found",
                       "tests": ["cargo test retries: passed"], "artifacts": ["README.md"]}
tasks[1]["checkpoints"] = [{"progress": "Empty state implemented", "decisions": "Use muted text",
                           "tried": "Unit tests passed", "remaining": "Narrow layout verification",
                           "next_step": "Check 40-column layout", "verification": "cargo test: passed"}]
directory = Path(os.environ["XDG_STATE_HOME"]) / "herdr-switchboard" / "tasks" / namespace
directory.mkdir(parents=True, exist_ok=True)
path = directory / "registry.json"
with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as stream:
    json.dump({"schema_version": 1, "sequence": 3, "tasks": tasks}, stream)
