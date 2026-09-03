//! `agora fake-agent`：测试用的假 agent（agora-3la；ADR-001 输入 ②）。
//!
//! 做成 agora 自己的子命令，跨平台、不依赖 bash；集成测试用 `env!("CARGO_BIN_EXE_agora")`
//! 拿到绝对路径塞进运行时。脚本是一行一条指令（文件或 `-e "a; b; c"`）：
//!
//! - `print <text>`：输出一行
//! - `sleep <ms>`：睡
//! - `read`：阻塞等一行 stdin，回显 `read:<line>`（EOF → 输出 `read:EOF` 并退出 0）
//! - `ignore-hup`：忽略 SIGHUP（模拟不理会挂断的 agent）
//! - `exit <code>`：以该码退出
//!
//! 脚本走完没有 `exit` 就退出 0。"被信号杀"由外面（terminate / kill）施加，这里不模拟。

use std::io::{BufRead, Write};

pub fn run(args: &[&str]) -> i32 {
    let script = match args {
        ["-e", inline] => inline
            .split(';')
            .map(|s| s.trim().to_owned())
            .collect::<Vec<_>>(),
        [path] => match std::fs::read_to_string(path) {
            Ok(s) => s.lines().map(|l| l.trim().to_owned()).collect(),
            Err(err) => {
                eprintln!("fake-agent: 读脚本 {path} 失败: {err}");
                return 2;
            }
        },
        _ => {
            eprintln!("用法: agora fake-agent <script-file> | -e \"print a; sleep 100; exit 0\"");
            return 2;
        }
    };
    let stdout = std::io::stdout();
    let stdin = std::io::stdin();
    for line in script
        .iter()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match cmd {
            "print" => {
                let mut out = stdout.lock();
                let _ = writeln!(out, "{arg}");
                let _ = out.flush();
            }
            "sleep" => {
                let ms: u64 = arg.parse().unwrap_or(0);
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
            "read" => {
                let mut buf = String::new();
                let mut out = stdout.lock();
                match stdin.lock().read_line(&mut buf) {
                    Ok(0) => {
                        let _ = writeln!(out, "read:EOF");
                        let _ = out.flush();
                        return 0;
                    }
                    Ok(_) => {
                        let _ = writeln!(out, "read:{}", buf.trim_end());
                        let _ = out.flush();
                    }
                    Err(err) => {
                        let _ = writeln!(out, "read:ERR {err}");
                        return 1;
                    }
                }
            }
            #[cfg(unix)]
            "ignore-hup" => {
                // SAFETY: 只把 SIGHUP 置为忽略，无其它副作用。
                unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
            }
            "exit" => return arg.parse().unwrap_or(0),
            other => {
                eprintln!("fake-agent: 未知指令 {other:?}");
                return 2;
            }
        }
    }
    0
}
