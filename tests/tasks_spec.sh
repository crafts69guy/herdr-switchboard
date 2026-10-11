#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo build --quiet

# Exercise the public executable with real Git/private state and a local Herdr
# stand-in. No personal server, agent, credentials or config is accessed.
python3 - "$ROOT/target/debug/herdr-switchboard" <<'PY'
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

binary = sys.argv[1]
with tempfile.TemporaryDirectory(prefix="swb-task-cli-") as temp:
    root = Path(temp)
    (root / "bin").mkdir()
    repo = root / "repo"
    repo.mkdir()
    env = {"PATH": str(root / "bin") + os.pathsep + os.environ["PATH"],
           "HOME": temp, "XDG_STATE_HOME": str(root / "state"), "XDG_CONFIG_HOME": str(root / "config"),
           "HERDR_ENV": "1", "HERDR_SOCKET_PATH": str(root / "h.sock"), "HERDR_PANE_ID": "w1:p1",
           "TASK_TEST_ROOT": temp, "GIT_CONFIG_NOSYSTEM": "1"}
    def git(*args):
        return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()
    git("init", "-q")
    (repo / "base.txt").write_text("base\n")
    git("add", ".")
    git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "base")
    base = git("rev-parse", "HEAD")
    (repo / "source-only.txt").write_text("must not be copied\n")
    herdr = root / "bin" / "herdr"
    herdr.write_text('''#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
r = Path(os.environ["TASK_TEST_ROOT"])
p = r / "herdr-state.json"
s = json.loads(p.read_text()) if p.exists() else {"pane": "w2:p1"}
a = sys.argv[1:]
def flag(k): return a[a.index(k)+1]
result = {}
if a[:2] == ["worktree", "create"]:
    subprocess.run(["git", "-C", flag("--cwd"), "worktree", "add", "-qb", flag("--branch"), str(r / "worktree"), flag("--base")], check=True)
    result = {"workspace": {"workspace_id": "w2"}, "root_pane": {"pane_id": s["pane"], "cwd": str(r / "worktree")}}
elif a[:2] == ["tab", "create"]:
    s["pane"] = "w2:p2"
    result = {"root_pane": {"pane_id": s["pane"], "cwd": str(r / "worktree")}}
elif a[:2] == ["workspace", "get"]:
    result = {"workspace": {"worktree": {"checkout_path": str(r / "worktree")}}}
elif a[:2] == ["agent", "start"]:
    s["name"] = a[2]
elif a[:2] == ["agent", "get"]:
    if a[2] != s.get("name"): sys.exit(1)
    result = {"agent": {"pane_id": s["pane"], "foreground_cwd": str(r / "worktree"), "agent_status": "idle", "agent_session": {"value": s["name"]}}}
elif a[:2] == ["pane", "current"]:
    result = {"pane": {"pane_id": os.environ["HERDR_PANE_ID"]}}
elif a[:2] == ["agent", "prompt"]:
    s["prompts"] = s.get("prompts", 0) + 1
else: sys.exit(2)
p.write_text(json.dumps(s))
print(json.dumps({"result": result}))
''')
    herdr.chmod(0o755)
    server = socket.socket(socket.AF_UNIX)
    server.bind(env["HERDR_SOCKET_PATH"])
    def cli(*args, ok=True, caller="w1:p1"):
        result = subprocess.run([binary, "task", *args], env={**env, "HERDR_PANE_ID": caller}, capture_output=True, text=True)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        response = json.loads(result.stdout if ok else result.stderr)
        assert response["schema_version"] == 1
        return response["result" if ok else "error"]
    def file(name, content):
        path = root / name
        path.write_text(json.dumps(content))
        return str(path)
    request = file("request.json", {"repo": str(repo), "goal": "Check CLI", "scope": "base.txt",
        "acceptance": "tests pass", "expected_result": "summary"})
    task = cli("delegate", "--file", request, "--kind", "codex")["task"]
    task_id = task["id"]
    assert task["base_commit"] == base and task["source_dirty"]
    assert not (root / "worktree/source-only.txt").exists()
    assert cli("claim", task_id, "--generation", "1", ok=False)["code"] == "not_owner"
    cli("claim", task_id, "--generation", "1", caller="w2:p1")
    (root / "worktree/unfinished.txt").write_text("preserve on handoff\n")
    checkpoint = file("checkpoint.json", {"progress": "part done", "decisions": "keep API", "tried": "tests",
        "remaining": "review", "next_step": "review", "verification": "baseline passed"})
    cli("release", task_id, "--generation", "1", "--file", checkpoint, caller="w2:p1")
    cli("handoff", task_id, "--kind", "claude")
    assert (root / "worktree/unfinished.txt").read_text() == "preserve on handoff\n"
    cli("claim", task_id, "--generation", "2", caller="w2:p2")
    report = file("result.json", {"status": "completed", "summary": "checked", "tests": ["smoke: passed"], "artifacts": []})
    assert cli("report", task_id, "--generation", "1", "--file", report, caller="w2:p1", ok=False)["code"] == "stale_generation"
    completed = cli("report", task_id, "--generation", "2", "--file", report, caller="w2:p2")["task"]
    assert completed["evidence"]["dirty"] and "unfinished.txt" in completed["evidence"]["changed_files"]
    assert cli("wait", task_id, "--timeout", "1")["task"]["status"] == "completed"
    assert len(cli("list")["tasks"]) == 1
    assert json.loads((root / "herdr-state.json").read_text())["prompts"] == 2
    registry = next((root / "state").rglob("registry.json"))
    assert registry.stat().st_mode & 0o777 == 0o600
    server.close()
print("tasks_spec: ok")
PY
