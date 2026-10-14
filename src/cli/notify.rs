//! Notification CLI: bounded input and one deadline, with uncertain submission kept distinct.
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::local::{self, Request, Response, SOCKET_FILE};
use crate::notify::{parse, MAX_INPUT};

pub const TIMEOUT: Duration = Duration::from_secs(10);
const USAGE: &str = "用法：agora notify [--home <dir>]（stdin 第一行为标题，其余为正文）";

#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub reason: String,
}

fn failure(code: i32, reason: impl ToString) -> Failure {
    Failure {
        code,
        reason: reason.to_string(),
    }
}

pub async fn submit(
    input: impl AsyncRead + Unpin,
    home: &Path,
    budget: Duration,
) -> Result<(), Failure> {
    let mut writing = false;
    let result = tokio::time::timeout(budget, async {
        let mut bytes = Vec::new();
        input
            .take((MAX_INPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| failure(1, format!("无法读取输入：{e}；请检查输入管道")))?;
        let (title, body) = parse(&bytes).map_err(|e| failure(1, e))?;
        let path = home.join(SOCKET_FILE);
        let mut stream = UnixStream::connect(&path).await.map_err(|e| {
            failure(
                1,
                format!(
                    "无法连接 agora（{}）：{e}；请检查路径、权限并启动 daemon",
                    path.display()
                ),
            )
        })?;
        let mut line = serde_json::to_vec(&Request::Notify { title, body }).expect("通知可序列化");
        line.push(b'\n');
        // 2026-10-14: even a failed write may have delivered a complete request. Never retry it.
        writing = true;
        stream.write_all(&line).await.map_err(|e| failure(4, e))?;
        let mut reply = String::new();
        BufReader::new(stream)
            .take(8193)
            .read_line(&mut reply)
            .await
            .map_err(|e| failure(4, e))?;
        if reply.len() > 8192 || !reply.ends_with('\n') {
            return Err(failure(4, "未收到完整应答"));
        }
        match serde_json::from_str::<Response>(&reply) {
            Ok(Response::Notified) => Ok(()),
            Ok(Response::NotifyRejected { reason }) => Err(failure(1, reason)),
            // Older daemons reject the unknown op before executing anything.
            Ok(Response::Error { message }) => Err(failure(1, message)),
            _ => Err(failure(4, "daemon 应答无法识别")),
        }
    })
    .await;
    result.unwrap_or_else(|_| {
        Err(failure(
            if writing { 4 } else { 1 },
            "通知命令超时；请检查输入管道与 daemon",
        ))
    })
}

pub async fn run(args: &[&str]) -> i32 {
    let home = match args {
        [] => local::resolve_home(),
        ["--home", home] if !home.is_empty() => (*home).into(),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match submit(tokio::io::stdin(), &home, TIMEOUT).await {
        Ok(()) => {
            println!("已发布通知");
            0
        }
        Err(err) => {
            let reason: String = err
                .reason
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
            if err.code == 4 {
                eprintln!("通知结果未知，可能已发布：{reason}；请检查 daemon 日志，不要自动重试");
            } else {
                eprintln!("{reason}");
            }
            err.code
        }
    }
}
