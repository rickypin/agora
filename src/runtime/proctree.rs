//! 进程树：给"未注册会话里跑的是哪个 agent"提供线索（MISSION §5.4）。pane 的进程是 shell，
//! agent 是它的后代，所以要拿整张 `ps` 表往下走。结论只是 hint：用户在 Adopt 时手填的
//! `agent_type` 优先，这里认错了也只是默认值错。

use std::time::Duration;

use super::exec::{exec, ExecOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub argv: Vec<String>,
}

/// `ps -axo pid=,ppid=,args=`：macOS 与 Linux procps 都认这组列名与 `=` 去表头的写法
/// （实测 2026-09-04 macOS 15 / ubuntu 24.04）。失败就当没有进程表——hint 缺席不算错。
pub fn process_table() -> Vec<Proc> {
    let opts = ExecOptions {
        timeout: Some(Duration::from_secs(3)),
        ..Default::default()
    };
    match exec(&["ps", "-axo", "pid=,ppid=,args="], &opts) {
        Ok(out) if out.status.success() => parse_table(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

pub fn parse_table(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let argv: Vec<String> = it.map(str::to_owned).collect();
            (!argv.is_empty()).then_some(Proc { pid, ppid, argv })
        })
        .collect()
}

/// `root` 的全部后代，按广度优先（离 pane 最近的先出）；不含 root 自己。
pub fn descendants(table: &[Proc], root: u32) -> Vec<&Proc> {
    let mut out: Vec<&Proc> = Vec::new();
    let mut frontier = vec![root];
    while let Some(pid) = frontier.pop() {
        for p in table.iter().filter(|p| p.ppid == pid) {
            if out.iter().any(|x| x.pid == p.pid) {
                continue; // 防 ps 表里的环（pid 复用）
            }
            out.push(p);
            frontier.insert(0, p.pid);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_down_from_the_pane_shell() {
        let t = parse_table(
            "  1     0 launchd\n 10     1 -zsh\n 20    10 node /opt/agentx/cli.js\n 30    20 sh -c hook\n 40     1 other\n",
        );
        let d = descendants(&t, 10);
        let pids: Vec<u32> = d.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![20, 30]);
        assert_eq!(d[0].argv, vec!["node", "/opt/agentx/cli.js"]);
        assert!(descendants(&t, 40).is_empty());
    }
}
