#!/bin/sh
# Regenerate ROADMAP.md from beads epics. ROADMAP.md is a VIEW — never edit it by hand.
# Usage: scripts/roadmap-view.sh   (run from anywhere inside the repo)
set -e
cd "$(git rev-parse --show-toplevel)"
python3 - <<'PY'
import json, subprocess, re, datetime
def run(*a):
    return subprocess.run(["bd", *a], capture_output=True, text=True, check=True).stdout
epics = json.loads(run("list", "-t", "epic", "--all", "--json") or "[]")
progress = {}
for line in run("epic", "status").splitlines():
    m = re.match(r"\s*\S\s+(\S+)\s", line)
    if m: cur = m.group(1)
    m = re.search(r"Progress:\s*(\d+)/(\d+)", line)
    if m: progress[cur] = f"{m.group(1)}/{m.group(2)}"
def blockers(eid):
    try:
        deps = json.loads(run("dep", "list", eid, "--json") or "[]")
        return [d.get("id") or d.get("depends_on_id") for d in deps if (d.get("dependency_type") or d.get("type")) == "blocks"]
    except Exception:
        out = run("dep", "list", eid)
        return [l.split(":")[0].strip() for l in out.splitlines() if "via blocks" in l]
epics.sort(key=lambda e: e["title"])
rows = []
scripts = []
for e in epics:
    title = e["title"]; phase, _, goal = title.partition(":")
    gate = ", ".join(f"`{b}`" for b in blockers(e["id"]) if b) or "—"
    acc = (e.get("acceptance_criteria") or "—").replace("\n", " ")
    rows.append(f"| {phase.strip()} | `{e['id']}` | {goal.strip() or title} | {gate} | {acc} | {e['status']} {progress.get(e['id'], '')} |")
    if (e.get("design") or "").strip():
        scripts.append(f"### {phase.strip()} `{e['id']}`\n\n{e['design'].strip()}\n")
today = datetime.date.today().isoformat()
doc = f"""# agora — ROADMAP（视图）

> **由 `scripts/roadmap-view.sh` 生成，不要手改。** 真相源是 beads：阶段 = epic，阶段门 = epic 之间的 `blocks` 依赖，验收标准 = epic 的 `--acceptance`，演示剧本 = epic 的 `--design`（下方"演示剧本"一节）。
> 本文件不放任务 checkbox（避免 devcenter 式双轨，见 `docs/analysis/beads/README.md` §6.3 / §8.2）。任务级细节：`bd ready`、`bd dep tree <epic>`。
> 生成时间：{today}

| 阶段 | epic | 目标 | 阶段门（被谁阻塞） | 验收要点 | 状态 / 进度 |
|---|---|---|---|---|---|
""" + "\n".join(rows) + "\n"
if scripts:
    doc += "\n## 演示剧本（epic 的 design 字段；人按此关闭 epic，MISSION §1.5）\n\n" + "\n".join(scripts)
open("ROADMAP.md", "w", encoding="utf-8").write(doc)
print(f"ROADMAP.md regenerated: {len(rows)} epic(s)")
PY
