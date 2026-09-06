#!/bin/sh
# agora 安装脚本（A26；ADR-001 D7、ADR-003 D6；docs/spec/config.md「安装」）。
#
# 一条命令把一台机器装成 agora 节点：校验 tmux ≥ 3.2（没有就 brew / apt 装）、建
# <AGORA_HOME>（0700）、首次写 config.yaml、把二进制放进 versions/<sha256 前 12 位>/agora 并让
# bin/agora 指向它、写随登录自启的 systemd 用户单元 / launchd 代理（LANG=C.UTF-8 写在单元里，
# 这是 devcenter CJK 乱码教训的另一半）。macOS 与 Ubuntu 24.04 都能跑；POSIX sh，不依赖 bash。
#
# 幂等：重跑不重复拷贝、不覆盖已有的 config.yaml、单元文件内容没变就不重写（mtime 不动）。
# 输出纪律：给人看的话一律走 stderr，stdout 留给将来的机器可读输出（MISSION §2.3 规则 10）。
#
# 用法见 usage()。测试守卫：tests/install_script.rs。

set -eu

MIN_TMUX_MAJOR=3
MIN_TMUX_MINOR=2
SERVICE_NAME=agora.service
LAUNCHD_LABEL=dev.agora.daemon
DEFAULT_PORT=7680
PORT_SCAN_LIMIT=20

say() { printf '%s\n' "$*" >&2; }
die() { say "install.sh: $*"; exit 1; }

usage() {
    cat >&2 <<'EOF'
用法: scripts/install.sh --binary <path> [选项]

  --binary <path>       要安装的 agora 二进制（必填；当前没有发布渠道，交叉编译后拷过来）
  --home <dir>          AGORA_HOME（默认 $AGORA_HOME，再默认 ~/.agora）
  --node-id <id>        node.id（默认短主机名；--home 不是默认路径时为 <主机名>-<用户名>）
  --listen <addr>       server.listen（默认 127.0.0.1:7680；未指定且被占则顺延到下一个空闲端口）
  --tls-listen <addr>   server.tls_listen（可选，例 0.0.0.0:7681；被 peer / 手机访问的节点才开）
  --tmux-socket <name>  runtime.tmux.socket（开发机上起第二个实例用；同时把 adopt_sockets 置空）
  --unit-dir <dir>      单元文件目录（默认 ~/.config/systemd/user 或 ~/Library/LaunchAgents）
  --no-service          只写单元文件，不 enable / bootstrap
  --skip-tmux           只校验 tmux 版本，不安装
  --dry-run             只打印将做的事，什么都不写
  -h, --help            本说明

只在 config.yaml 不存在时写它；二进制按 sha256 放到 <home>/versions/<sha 前 12 位>/agora，
<home>/bin/agora 是指向它的符号链接（hook 与升级都依赖这条稳定路径）。
EOF
}

# ---------- 参数 ----------
BINARY=
HOME_DIR=
NODE_ID=
LISTEN=
TLS_LISTEN=
TMUX_SOCKET=
UNIT_DIR=
NO_SERVICE=0
SKIP_TMUX=0
DRY_RUN=0

need_arg() { [ $# -ge 2 ] || die "$1 需要一个参数"; }
while [ $# -gt 0 ]; do
    case "$1" in
        --binary) need_arg "$@"; BINARY=$2; shift 2 ;;
        --home) need_arg "$@"; HOME_DIR=$2; shift 2 ;;
        --node-id) need_arg "$@"; NODE_ID=$2; shift 2 ;;
        --listen) need_arg "$@"; LISTEN=$2; shift 2 ;;
        --tls-listen) need_arg "$@"; TLS_LISTEN=$2; shift 2 ;;
        --tmux-socket) need_arg "$@"; TMUX_SOCKET=$2; shift 2 ;;
        --unit-dir) need_arg "$@"; UNIT_DIR=$2; shift 2 ;;
        --no-service) NO_SERVICE=1; shift ;;
        --skip-tmux) SKIP_TMUX=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage; die "不认识的参数: $1" ;;
    esac
done
[ -n "$BINARY" ] || { usage; die "--binary 必填"; }
[ -f "$BINARY" ] || die "--binary 不是文件: $BINARY"

OS=$(uname -s)
case "$OS" in
    Darwin|Linux) ;;
    *) die "不支持的系统: ${OS}（只支持 macOS 与 Linux）" ;;
esac

: "${HOME:?install.sh: HOME 未设置}"
USER_NAME=${USER:-$(id -un)}
HOME_DEFAULT=$HOME/.agora
[ -n "$HOME_DIR" ] || HOME_DIR=${AGORA_HOME:-$HOME_DEFAULT}

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd -P)
TEMPLATE_DIR=$SCRIPT_DIR/templates
[ -d "$TEMPLATE_DIR" ] || die "找不到模板目录 ${TEMPLATE_DIR}（请把整个 scripts/ 目录一起拷过来）"

# 目录的绝对路径（不要求已存在：取最深的已存在祖先）。macOS 的 /tmp 是 /private/tmp 的链接，
# 链接目标与测试里的 canonicalize 都要按真实路径比，所以走 pwd -P。
abs_dir() {
    _p=$1
    _suffix=
    while [ ! -d "$_p" ]; do
        _suffix=/$(basename "$_p")$_suffix
        _p=$(dirname "$_p")
    done
    printf '%s%s\n' "$(cd "$_p" && pwd -P)" "$_suffix"
}
HOME_DIR=$(abs_dir "$HOME_DIR")
HOME_DEFAULT_ABS=$(abs_dir "$HOME_DEFAULT")

# 只允许 node.id 的字符集（src/config.rs）：字母、数字、- 与 _；其余换成 -。
sanitize_id() { printf '%s' "$1" | tr -c 'A-Za-z0-9_-' '-' | sed 's/^-*//; s/-*$//'; }
if [ -z "$NODE_ID" ]; then
    host=$(hostname -s 2>/dev/null || uname -n)
    host=$(sanitize_id "${host%%.*}")
    [ -n "$host" ] || host=node
    # 同机多实例（--home 不是默认路径）时把用户名带上，让 <node>:<id> 在 peer 眼里不撞。
    if [ "$HOME_DIR" != "$HOME_DEFAULT_ABS" ]; then
        NODE_ID=$host-$(sanitize_id "$USER_NAME")
    else
        NODE_ID=$host
    fi
fi
case "$NODE_ID" in
    *[!A-Za-z0-9_-]*|'') die "--node-id 只能含字母、数字、- 与 _: $NODE_ID" ;;
esac

# ---------- 工具 ----------
run() {
    # 有副作用的命令都经这里：--dry-run 只打印。
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] $*"
    else
        "$@"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "找不到 sha256sum / shasum"
    fi
}

# sed 替换值里的 \ & | 要转义（路径里几乎不会出现，但出现了不能悄悄写坏单元文件）。
sed_escape() { printf '%s' "$1" | sed 's/[\\&|]/\\&/g'; }

# 端口探测只用于挑默认端口；哪个工具都没有就当空闲（daemon 起不来会自己报错退出，ADR-003 D6）。
port_in_use() {
    _host=$1
    _port=$2
    if command -v nc >/dev/null 2>&1; then
        nc -z "$_host" "$_port" >/dev/null 2>&1
    elif command -v ss >/dev/null 2>&1; then
        ss -Hltn 2>/dev/null | awk '{print $4}' | grep -q ":$_port\$"
    elif command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$_port" -sTCP:LISTEN >/dev/null 2>&1
    elif command -v bash >/dev/null 2>&1; then
        bash -c "exec 3<>/dev/tcp/$_host/$_port" >/dev/null 2>&1
    else
        return 1
    fi
}

# ---------- ① tmux ≥ 3.2 ----------
# 版本按 major.minor 数值比，不按字符串顺序：`tmux 3.7c`、`tmux 3.2a`、`tmux next-3.4` 都要能解析，
# 将来的 `tmux 10.0` 也不能被当成比 3.2 小。
tmux_version() {
    _out=$(tmux -V 2>/dev/null | head -n 1)
    [ -n "$_out" ] && printf '%s\n' "$_out"
}
tmux_ok() {
    _v=$(tmux_version) || return 1
    _num=$(printf '%s\n' "$_v" | sed -n 's/^[^0-9]*\([0-9][0-9]*\)\.\([0-9][0-9]*\).*$/\1 \2/p')
    [ -n "$_num" ] || return 1
    _major=${_num% *}
    _minor=${_num#* }
    [ "$_major" -gt "$MIN_TMUX_MAJOR" ] ||
        { [ "$_major" -eq "$MIN_TMUX_MAJOR" ] && [ "$_minor" -ge "$MIN_TMUX_MINOR" ]; }
}
tmux_install_cmd() {
    if [ "$OS" = Darwin ]; then
        printf '%s\n' "HOMEBREW_NO_AUTO_UPDATE=1 brew install tmux"
    else
        printf '%s\n' "sudo apt-get update && sudo apt-get install -y tmux"
    fi
}

if tmux_ok; then
    say "tmux: $(tmux_version)（≥ $MIN_TMUX_MAJOR.${MIN_TMUX_MINOR}）"
else
    have=$(tmux_version || printf '未安装')
    if [ "$SKIP_TMUX" = 1 ]; then
        die "tmux 不满足 ≥ $MIN_TMUX_MAJOR.${MIN_TMUX_MINOR}（现在: ${have}）；--skip-tmux 下不安装，请手动执行: $(tmux_install_cmd)"
    fi
    say "tmux 不满足 ≥ $MIN_TMUX_MAJOR.${MIN_TMUX_MINOR}（现在: ${have}），安装…"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] $(tmux_install_cmd)"
    else
        installed=0
        if [ "$OS" = Darwin ]; then
            if command -v brew >/dev/null 2>&1; then
                HOMEBREW_NO_AUTO_UPDATE=1 brew install tmux >&2 && installed=1
            fi
        elif command -v apt-get >/dev/null 2>&1; then
            # -n：sudo 要密码就直接失败，把命令打给人，不在脚本里等输入。
            if sudo -n apt-get update >&2 && sudo -n apt-get install -y tmux >&2; then
                installed=1
            fi
        fi
        [ "$installed" = 1 ] || die "装不了 tmux，请手动执行: $(tmux_install_cmd)"
        if ! tmux_ok; then
            die "装完的 tmux 仍低于 $MIN_TMUX_MAJOR.${MIN_TMUX_MINOR}（$(tmux_version || printf '未安装')）；请从别的渠道装新版再重跑"
        fi
        say "tmux: $(tmux_version)"
    fi
fi

# ---------- ② AGORA_HOME 与 config.yaml ----------
CONFIG=$HOME_DIR/config.yaml
if [ -d "$HOME_DIR" ]; then
    say "AGORA_HOME: ${HOME_DIR}（已存在）"
    # 权限过宽 daemon 会拒绝启动（ADR-003 D6）；安装脚本顺手收紧。
    perms=$(stat -f '%Lp' "$HOME_DIR" 2>/dev/null || stat -c '%a' "$HOME_DIR")
    [ "$perms" = 700 ] || run chmod 700 "$HOME_DIR"
else
    say "AGORA_HOME: ${HOME_DIR}（新建，0700）"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] mkdir -p -m 700 $HOME_DIR"
    else
        # 中间目录用默认权限，最后一级 0700；mkdir -m 只作用于最后一级。
        mkdir -p "$(dirname "$HOME_DIR")"
        mkdir -m 700 "$HOME_DIR"
    fi
fi

if [ -f "$CONFIG" ]; then
    say "config.yaml: 已存在，不动（${CONFIG}）"
else
    host_part=127.0.0.1
    if [ -z "$LISTEN" ]; then
        port=$DEFAULT_PORT
        tls_port=${TLS_LISTEN##*:}
        n=0
        while [ "$n" -lt "$PORT_SCAN_LIMIT" ]; do
            if [ "$port" != "$tls_port" ] && ! port_in_use "$host_part" "$port"; then
                break
            fi
            port=$((port + 1))
            n=$((n + 1))
        done
        [ "$n" -lt "$PORT_SCAN_LIMIT" ] || die "从 $DEFAULT_PORT 起 $PORT_SCAN_LIMIT 个端口都被占，请用 --listen 指定"
        LISTEN=$host_part:$port
        if [ "$port" != "$DEFAULT_PORT" ]; then
            say "端口 $DEFAULT_PORT 已被占用，server.listen 改用 ${LISTEN}（agora url 会跟着它）"
        fi
    fi
    # 两个监听器必须不同端口（src/config.rs 会拒绝）；写进文件之前就拦下，别让人装完才发现起不来。
    if [ -n "$TLS_LISTEN" ] && [ "${LISTEN##*:}" = "${TLS_LISTEN##*:}" ]; then
        die "--listen 与 --tls-listen 端口相同（${LISTEN##*:}）：两个监听器必须不同端口"
    fi
    tls_line=
    [ -z "$TLS_LISTEN" ] || tls_line=$(printf '  tls_listen: "%s"\n' "$TLS_LISTEN")
    runtime_block=
    if [ -n "$TMUX_SOCKET" ]; then
        runtime_block=$(printf 'runtime:\n  tmux:\n    socket: "%s"\n    adopt_sockets: []   # 第二个实例：不扫用户默认 socket\n' "$TMUX_SOCKET")
    fi
    say "config.yaml: 写入 node.id=$NODE_ID server.listen=$LISTEN${TLS_LISTEN:+ server.tls_listen=$TLS_LISTEN}${TMUX_SOCKET:+ runtime.tmux.socket=$TMUX_SOCKET}"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] write $CONFIG"
    else
        tmp=$CONFIG.tmp
        umask 077
        {
            printf '# 由 scripts/install.sh 生成；其余键留默认，全部键见 docs/spec/config.md\n'
            printf 'server:\n'
            printf '  listen: "%s"\n' "$LISTEN"
            [ -z "$tls_line" ] || printf '%s\n' "$tls_line"
            printf 'node:\n'
            printf '  id: "%s"\n' "$NODE_ID"
            [ -z "$runtime_block" ] || printf '%s\n' "$runtime_block"
        } >"$tmp"
        mv -f "$tmp" "$CONFIG"
    fi
fi

# ---------- ③ 二进制：versions/<sha12>/agora + bin/agora 链接 ----------
sha=$(sha256_of "$BINARY")
sha12=$(printf '%s' "$sha" | cut -c1-12)
VERSION_DIR=$HOME_DIR/versions/$sha12
TARGET=$VERSION_DIR/agora
LINK=$HOME_DIR/bin/agora
if [ -f "$TARGET" ] && [ "$(sha256_of "$TARGET")" = "$sha" ]; then
    say "binary: versions/$sha12/agora 已是同一份，复用"
else
    say "binary: $BINARY → versions/$sha12/agora"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] install -m 755 $BINARY $TARGET"
    else
        mkdir -p "$VERSION_DIR"
        # 先拷到 .tmp 再 mv：中途断电不留半个二进制在正式路径上。
        cp -f "$BINARY" "$TARGET.tmp"
        chmod 755 "$TARGET.tmp"
        mv -f "$TARGET.tmp" "$TARGET"
    fi
fi
# 链接已指向同一目标就不重做——ln -sfn 会换一个新的链接文件，没必要。
current=$(readlink "$LINK" 2>/dev/null || true)
if [ "$current" = "$TARGET" ]; then
    say "bin/agora → ${TARGET}（已是）"
else
    say "bin/agora → $TARGET"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] ln -sfn $TARGET $LINK"
    else
        mkdir -p "$HOME_DIR/bin"
        ln -sfn "$TARGET" "$LINK"
    fi
fi

# ---------- ④ 随登录自启 ----------
# 单元里的 PATH 只要够找到 tmux 与二进制：daemon 起来后自己探测用户 shell 的完整 PATH（ADR-001 D7）。
# tmux 装在这几个目录之外（linuxbrew、nix）就把它所在的目录也加上。
if [ "$OS" = Darwin ]; then
    UNIT_PATH=$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin
else
    UNIT_PATH=$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin
fi
tmux_bin=$(command -v tmux 2>/dev/null || true)
if [ -n "$tmux_bin" ]; then
    tmux_dir=$(dirname "$tmux_bin")
    case ":$UNIT_PATH:" in
        *":$tmux_dir:"*) ;;
        *) UNIT_PATH=$tmux_dir:$UNIT_PATH ;;
    esac
fi

render_template() {
    sed -e "s|@AGORA_HOME@|$(sed_escape "$HOME_DIR")|g" \
        -e "s|@PATH@|$(sed_escape "$UNIT_PATH")|g" \
        "$1"
}

# 内容没变就不碰文件（保住 mtime，也让 systemd 的 daemon-reload 少一次没意义的重载）。
write_if_changed() {
    _dst=$1
    _content=$2
    if [ -f "$_dst" ] && [ "$(cat "$_dst")" = "$_content" ]; then
        say "unit: $_dst 未变"
        return 0
    fi
    say "unit: 写入 $_dst"
    if [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] write $_dst"
    else
        mkdir -p "$(dirname "$_dst")"
        printf '%s\n' "$_content" >"$_dst.tmp"
        mv -f "$_dst.tmp" "$_dst"
    fi
}

if [ "$OS" = Darwin ]; then
    [ -n "$UNIT_DIR" ] || UNIT_DIR=$HOME/Library/LaunchAgents
    PLIST=$UNIT_DIR/$LAUNCHD_LABEL.plist
    write_if_changed "$PLIST" "$(render_template "$TEMPLATE_DIR/$LAUNCHD_LABEL.plist")"
    if [ "$NO_SERVICE" = 1 ]; then
        say "service: --no-service，未 bootstrap；之后手动: launchctl bootstrap gui/$(id -u) $PLIST"
    elif [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] launchctl bootstrap gui/$(id -u) ${PLIST}（已加载则 kickstart -k）"
    else
        domain=gui/$(id -u)
        if launchctl print "$domain/$LAUNCHD_LABEL" >/dev/null 2>&1; then
            say "service: $LAUNCHD_LABEL 已加载，kickstart -k 让它读新单元"
            launchctl kickstart -k "$domain/$LAUNCHD_LABEL" >&2
        else
            launchctl bootstrap "$domain" "$PLIST" >&2
            say "service: $LAUNCHD_LABEL 已 bootstrap（登录即起）"
        fi
    fi
else
    [ -n "$UNIT_DIR" ] || UNIT_DIR=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
    UNIT=$UNIT_DIR/$SERVICE_NAME
    write_if_changed "$UNIT" "$(render_template "$TEMPLATE_DIR/$SERVICE_NAME")"
    if ! command -v systemctl >/dev/null 2>&1; then
        # 容器 / 没有 systemd 的发行版：单元文件照写，启用交给有 systemd 的地方。
        say "service: 本机没有 systemctl，跳过 enable（单元文件已写到 ${UNIT}）"
    elif [ "$NO_SERVICE" = 1 ]; then
        say "service: --no-service，未 enable；之后手动: systemctl --user daemon-reload && systemctl --user enable --now $SERVICE_NAME"
    elif [ "$DRY_RUN" = 1 ]; then
        say "[dry-run] systemctl --user daemon-reload && systemctl --user enable --now $SERVICE_NAME"
    else
        systemctl --user daemon-reload >&2
        systemctl --user enable --now "$SERVICE_NAME" >&2
        say "service: $SERVICE_NAME 已 enable --now"
    fi
    # 用户单元随登录起、随登出停；要开机自起并在无人登录时活着，得给这个用户开 linger。
    # loginctl 不自己 sudo：装软件与改系统设置的每一步都由人敲（ADR-003 的信任边界）。
    if command -v loginctl >/dev/null 2>&1; then
        linger=$(loginctl show-user "$USER_NAME" -p Linger --value 2>/dev/null || true)
        if [ "$linger" != yes ]; then
            say "提示: 用户 $USER_NAME 未开 linger，重启后 daemon 要等你登录才起；执行: sudo loginctl enable-linger $USER_NAME"
        fi
    fi
    for conf in /etc/systemd/logind.conf /etc/systemd/logind.conf.d/*.conf; do
        [ -f "$conf" ] || continue
        if grep -Eq '^[[:space:]]*KillUserProcesses[[:space:]]*=[[:space:]]*(yes|true|1)' "$conf"; then
            say "警告: $conf 设了 KillUserProcesses=yes，登出会杀掉用户进程（含 daemon 与 tmux server）；建议改回 no 并 sudo systemctl restart systemd-logind"
        fi
    done
    # ---------- ⑤ sshd ----------
    # ADR-003 的兜底是"ssh 上去 agora pair"；没有 sshd 就只能物理登录，这里只提示不装。
    if command -v systemctl >/dev/null 2>&1; then
        if ! systemctl is-active --quiet ssh 2>/dev/null && ! systemctl is-active --quiet sshd 2>/dev/null; then
            say "提示: sshd 未运行，远端只能物理登录后 agora pair；要开: sudo apt-get install -y openssh-server && sudo systemctl enable --now ssh"
        fi
    fi
fi

# ---------- ⑥ 下一步 ----------
say ""
if [ "$DRY_RUN" = 1 ]; then
    say "dry-run 结束，什么都没写。"
else
    say "装好了。下一步："
    say "  $LINK url        # 铸一条配对链接，在浏览器里打开"
    if [ "$NO_SERVICE" = 1 ]; then
        say "  AGORA_HOME=$HOME_DIR $LINK serve   # 没启用自启时手动起 daemon"
    fi
fi
