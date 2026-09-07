#!/bin/sh
# 文档引用一致性检查（agora-xqa.2）。两轮人工全文档修订的返工根因都是引用漂移——
# 章节被重排、A 编号被复制走样、文件被改名——这三样机器可查，就不该再花人的眼睛。
#
#   ① §引用：文档里写的 §N / §N.M 在被引的文档里真的有这一节。
#   ② A 编号：各 epic 的 --acceptance 引用的 A 编号 ⊆ MISSION §12 ∪ §11，且不重不漏。
#   ③ 相对链接与仓内路径：markdown 链接、以及正文里反引号包住的本仓库路径，目标存在。
#      指名道姓提到别的项目（FOREIGN）的那一行，名字之后的路径当作别家的不查；
#      但 docs/analysis/ 下的路径永远按本仓库的查——那是本仓库放别家分析报告的地方。
#   ④ 延期措辞：代码与自有文档里"归 M2 / 留 V2 / V1 只…"这样的句子，同一行必须带 agora-<id>
#      （文档里 §11 也算——它是 MISSION 的 V2 台账；§11 本节自己不查）。延期必须可追踪，MISSION §1.5。
#
# 用法：scripts/doc-lint.sh（仓库内任意目录）。有问题时逐条打印并以 1 退出。
# ② 需要 bd（beads 的库不进 git），没有 bd 时只跳过这一项、其余照跑——CI 就是这样。
set -e
cd "$(git rev-parse --show-toplevel)"
python3 - <<'PY'
import json, os, re, shutil, subprocess, sys

# 本仓库自己的文档：这些文档里的 §引用与路径引用都由本脚本负责。
OWN_DOCS = ["MISSION.md", "README.md", "ROADMAP.md", "AGENTS.md"]
OWN_DOCS += sorted(f"docs/spec/{f}" for f in os.listdir("docs/spec") if f.endswith(".md"))
OWN_DOCS += sorted(f"docs/adr/{f}" for f in os.listdir("docs/adr") if f.endswith(".md"))
# docs/analysis/ 是冻结的历史报告，章节号是它自己的、路径多指向被分析的那些仓库，不在此列。

# 引用别的项目时会指名道姓；这些名字之后的路径不是本仓库的东西（例：devcenter `src/which.rs`）。
# 例外的例外：以 ANALYSIS 开头的路径永远按本仓库的文件查——docs/analysis/<项目名>/ 是本仓库存放
# 对别的项目的分析报告的地方，出现在那里的路径按定义是本仓库的文件，哪怕句子在谈 devcenter。
# 2026-09-06 的反例：architecture.md 把 docs/analysis/devcenter/appendix-b-multihost.md 写成
# appendix-b.md，同一行前面有"devcenter 教训"，整行被放过、报 0 问题（agora-yuk）。
# 没有改成"以本仓库顶层目录开头就不例外"——ADR-001 引的 devcenter `src/which.rs` 会被报成缺失；
# 也没有改成"只放过紧跟在名字后面的那一个路径"——ADR-001 同一行隔着"；"的 `src/tmux.rs` 也是 devcenter 的。
FOREIGN = ["devcenter"]
ANALYSIS = "docs/analysis/"

# 允许跨 epic 共担的 A 编号——MISSION §12 自己写明了它们横跨阶段。
# 声明必须与现实一致：不在这里的重叠是错，在这里却没重叠了也是错（防止条目烂在脚本里）。
SHARED_A = {
    "A1": "已登记会话的展示在 M1a，未登记会话随 A22 归 M1b（MISSION §12 A1 行）",
    "A36": "不变量的守卫测试按阶段分批钉死（1–5/7 在 M1a、10 在 M1b、8/11 在 M2b）",
}

errors = []
def err(where, msg):
    errors.append(f"{where}: {msg}")

def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()

# ---------- 各文档有哪些节号 ----------
HEADING = re.compile(r"^#{1,6}\s+(\d+(?:\.\d+)*)[.、．]?\s")
def sections(path):
    return {m.group(1) for m in (HEADING.match(l) for l in read(path).splitlines()) if m}

section_cache = {}
def sections_of(path):
    if path not in section_cache:
        section_cache[path] = sections(path) if os.path.isfile(path) else None
    return section_cache[path]

# ---------- ① §引用 ----------
# 归属规则：§ 紧跟在某个 .md 文件名之后（中间只隔标点空白）就是引那篇，否则引 MISSION；
# 同一串里后续的 § 沿用前一个的归属（“`…/beads/README.md` §6.3 / §8.2”是一串两条）。
# 只核对本仓库自己的文档（OWN_DOCS + MISSION）；docs/analysis/ 是冻结的历史报告，
# 它的 § 是自己的编号习惯（甚至用来指行号，见 ADR-003 引附录 B 的 §54–58），不归本脚本管。
MD_MENTION = re.compile(r"[A-Za-z0-9_./-]+\.md|MISSION")
SECTION_REF = re.compile(r"§(\d+(?:\.\d+)*)")
GAP = re.compile(r"^[\s`、,，/·和与见的第节章]*$")
CHAIN = re.compile(r"^[^。；;]{0,16}$")  # 一串里的后续 §（“§54–58、§96”“§6 的… 与 §2.5”）
CHECKED = set(OWN_DOCS) | {"MISSION.md"}

def resolve(doc, name):
    if name == "MISSION":
        return "MISSION.md"
    if os.path.isfile(name):
        return name
    cand = os.path.normpath(os.path.join(os.path.dirname(doc), name))
    return cand if os.path.isfile(cand) else name

for doc in OWN_DOCS:
    for n, line in enumerate(read(doc).splitlines(), 1):
        marks = [(m.end(), resolve(doc, m.group(0))) for m in MD_MENTION.finditer(line)]
        prev_end, prev_target = None, None
        for m in SECTION_REF.finditer(line):
            target = "MISSION.md"
            near = [(e, t) for e, t in marks if e <= m.start()]
            if near and GAP.match(line[near[-1][0]:m.start()]):
                target = near[-1][1]
            elif prev_end is not None and CHAIN.match(line[prev_end:m.start()]):
                target = prev_target
            prev_end, prev_target = m.end(), target
            if target not in CHECKED:
                continue
            have = sections_of(target)
            if have is None:
                err(f"{doc}:{n}", f"§{m.group(1)} 指向的文档不存在: {target}")
            elif m.group(1) not in have:
                err(f"{doc}:{n}", f"{target} 没有 §{m.group(1)}（章节号一经引用不得重排，MISSION 文首编号冻结）")

# ---------- ② A 编号 ----------
PAREN = re.compile(r"[（(][^（()）]*[)）]")
def a_numbers(text):
    """A6–A12 这样的区间展开；‘不变量 1–5’没有 A 前缀，不会被当成编号。
    括号里的编号是解释（“A22 的前置”），不是认领——认领要写在正文里。"""
    while PAREN.search(text):
        text = PAREN.sub(" ", text)
    got = set()
    for lo, hi in re.findall(r"(?<![A-Za-z0-9])A(\d+)\s*[–—-]\s*A?(\d+)", text):
        got |= {f"A{i}" for i in range(int(lo), int(hi) + 1)}
    got |= {f"A{n}" for n in re.findall(r"(?<![A-Za-z0-9])A(\d+)\b", text)}
    return got

mission = read("MISSION.md")
universe = {f"A{n}" for n in re.findall(r"^- \[.\] \*\*A(\d+)\*\*", mission, re.M)}
if not universe:
    err("MISSION.md", "§12 / §11 里没找到任何 `- [ ] **A<n>**` 验收条目，A 编号检查失去基准")

if shutil.which("bd") is None:
    print("doc-lint: 没有 bd，跳过 ② epic 验收编号检查（beads 的库不进 git）", file=sys.stderr)
else:
    out = subprocess.run(["bd", "list", "-t", "epic", "--all", "--json"],
                         capture_output=True, text=True, check=True).stdout
    claims = {}
    for e in json.loads(out or "[]"):
        got = a_numbers(e.get("acceptance_criteria") or "")
        if got:
            claims[e["id"]] = got
    seen = {}
    for eid, got in sorted(claims.items()):
        for a in sorted(got, key=lambda x: int(x[1:])):
            if a not in universe:
                err(f"beads {eid}", f"验收引用了 MISSION 里没有的编号 {a}")
            seen.setdefault(a, []).append(eid)
    for a, owners in sorted(seen.items(), key=lambda kv: int(kv[0][1:])):
        if len(owners) > 1 and a not in SHARED_A:
            err("beads", f"{a} 被多个 epic 认领（{', '.join(owners)}）；确实要共担就写进 doc-lint.sh 的 SHARED_A")
    for a, why in SHARED_A.items():
        if a in universe and len(seen.get(a, [])) < 2:
            err("scripts/doc-lint.sh", f"SHARED_A 里的 {a} 已经不再跨 epic（{why}），删掉这条声明")
    for a in sorted(universe - set(seen), key=lambda x: int(x[1:])):
        err("beads", f"{a} 没有任何 epic 认领（MISSION §12 的验收要么被某个阶段接走，要么删掉）")

# ---------- ③ 链接与仓内路径 ----------
all_md = subprocess.run(["git", "ls-files", "*.md"], capture_output=True, text=True,
                        check=True).stdout.split()
# 符号链接（CLAUDE.md → AGENTS.md）只看一次。
all_md = [f for f in all_md if not os.path.islink(f)]
LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
TOP = {e for e in os.listdir(".") if not e.startswith(".")} | {".github", ".beads"}
CODEPATH = re.compile(r"`([A-Za-z0-9_][A-Za-z0-9_./-]*\.[A-Za-z0-9]+)`")
for doc in all_md:
    for n, line in enumerate(read(doc).splitlines(), 1):
        for target in LINK.finditer(line):
            t = target.group(1).split("#")[0].strip()
            if not t or re.match(r"^[a-z]+:", t) or t.startswith("//"):
                continue
            if not os.path.exists(os.path.normpath(os.path.join(os.path.dirname(doc), t))):
                err(f"{doc}:{n}", f"链接目标不存在: {t}")
        if doc not in OWN_DOCS:
            continue
        for m in CODEPATH.finditer(line):
            t = m.group(1)
            if "/" not in t or t.split("/")[0] not in TOP:
                continue  # 不带目录的裸文件名（`config.yaml`）说的是形态，不是仓里某个文件
            before = line[: m.start()].lower()
            if not t.startswith(ANALYSIS) and any(name in before for name in FOREIGN):
                continue  # 指名道姓引别的项目的文件（devcenter `src/which.rs`）；docs/analysis/ 下的是我们自己的报告，照查
            if not os.path.exists(t):
                err(f"{doc}:{n}", f"引用的仓内路径不存在: {t}")

# ---------- ④ 延期措辞 ----------
# 延期必须可追踪（MISSION §1.5；agora-9xs）。2026-09-07 的反例：agora-xqa.12 把 New Agent 的 Node
# 下拉禁掉，注释只写"peer 归 M2"、没立 issue；两天后拆 M2 只按 §12 的 A 编号机械核对，而 §6.4 那句
# "本机 + 在线的 peer"没有 A 编号——M2a / M2b 都关了，Mac 上仍不能在 zuan 起会话（agora-fna）。
# 这里把"知道自己在延期"的那一刻钉死：写下延期措辞的那一行必须带 agora-<id>，文档里另可用 §11
# （MISSION 的 V2 台账）代替；§11 本节自己就是台账，不查。ROADMAP.md 是 beads 生成的视图，不查。
# 代码用宽模式（含"V1 只"——代码注释里这三个字只会是延期）；文档用窄模式，"transcript V1 只存不读"
# 这类设计陈述不算延期。别把模式收窄到只剩"归 M"：2026-09-07 的七处命中里五处是"V1 只 / 归 V2-1"。
DEFER_CODE = re.compile(r"归 M\d|归 V2|留 V2|留给 V2|留到 V2|放 V2|V1 只|V1 延期|延期到|暂不做|先不做")
DEFER_DOC = re.compile(r"归 M\d|归 V2|留 V2|留给 V2|留到 V2|放 V2|V1 延期|延期到|暂不做|先不做")
ISSUE_ID = re.compile(r"agora-[a-z0-9]{3,}(?:\.\d+)?")
DEFER_HINT = "延期必须可追踪：同一行写上承接它的 issue id（agora-xxx）"

def section_11_lines(path):
    """MISSION §11 的行号范围（含标题行）；没有 §11 就是空集。"""
    lines = read(path).splitlines()
    start = next((i for i, l in enumerate(lines, 1) if re.match(r"^##\s+11[.、．]?\s", l)), None)
    if start is None:
        return set()
    end = next((i for i, l in enumerate(lines, 1) if i > start and re.match(r"^##\s", l)), len(lines) + 1)
    return set(range(start, end))

code_files = subprocess.run(["git", "ls-files", "src", "web/src"], capture_output=True, text=True,
                            check=True).stdout.split()
code_files = [f for f in code_files if re.search(r"\.(rs|ts|tsx)$", f) and ".test." not in f]
for f in code_files:
    for n, line in enumerate(read(f).splitlines(), 1):
        if DEFER_CODE.search(line) and not ISSUE_ID.search(line):
            err(f"{f}:{n}", f"{DEFER_HINT}: {line.strip()[:80]}")
for doc in OWN_DOCS:
    if doc == "ROADMAP.md":
        continue
    skip = section_11_lines(doc) if doc == "MISSION.md" else set()
    for n, line in enumerate(read(doc).splitlines(), 1):
        if n in skip:
            continue
        if DEFER_DOC.search(line) and not ISSUE_ID.search(line) and "§11" not in line:
            err(f"{doc}:{n}", f"{DEFER_HINT}，或指向 MISSION §11: {line.strip()[:80]}")

for e in errors:
    print(e)
print(f"doc-lint: {len(OWN_DOCS)} 篇自有文档、{len(all_md)} 篇 md、{len(universe)} 条 A 编号；"
      f"{len(errors)} 个问题")
sys.exit(1 if errors else 0)
PY
