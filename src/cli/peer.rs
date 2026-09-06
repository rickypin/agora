//! `agora peer token create <name> [--rotate] | list | revoke <name>`（ADR-003 D3；agora-7ku.2）。
//!
//! 在**被访问**的节点上运行，直接操作 `AGORA_HOME/agora.db`，不需要 daemon 在跑（ADR-003 D6）；
//! daemon 若在跑，它每次请求都查库，所以这里的吊销 / 轮换对它即时生效。明文 token 只在
//! `create` 的 stdout 上出现一次，且 stdout 只有它一行——`agora peer token create mac > mac.token`
//! 得到的就是能直接当 `token_file` 的文件；提示走 stderr。

use crate::auth::peer_token::{self, PeerToken, PeerTokenError};
use crate::local;
use crate::session::Db;

const USAGE: &str =
    "用法: agora peer token create <name> [--rotate] | agora peer token list | agora peer token revoke <name>";

/// 退出码：0 成功、1 操作失败、2 用法错误（与 `agora hooks` 一致）。
pub fn run(args: &[&str]) -> i32 {
    match args {
        ["token", rest @ ..] => token(rest),
        _ => usage(),
    }
}

fn token(args: &[&str]) -> i32 {
    match args {
        ["create", name] => with_db(|db| create(db, name, false)),
        ["create", name, "--rotate"] | ["create", "--rotate", name] => {
            with_db(|db| create(db, name, true))
        }
        ["list"] => with_db(list),
        ["revoke", name] => with_db(|db| revoke(db, name)),
        _ => usage(),
    }
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

/// 目录自检（0700、属主）与打开库；任一步失败打印原因退出 2 / 1。库文件若由这里首次创建，
/// 也只能属主可读：与 daemon 同样先收 umask（umask 是进程级的，tokio 的 worker 线程已在跑也
/// 没关系——它们不在同一时刻创建文件）。
fn with_db(f: impl FnOnce(&Db) -> Result<(), PeerTokenError>) -> i32 {
    // SAFETY: umask 没有前置条件。
    unsafe { libc::umask(0o077) };
    let home = local::resolve_home();
    if let Err(err) = local::ensure_home(&home) {
        eprintln!("{err}");
        return 2;
    }
    let db = match Db::open(&home.join("agora.db")) {
        Ok(db) => db,
        Err(err) => {
            eprintln!("打开 metadata 库失败: {err}");
            return 1;
        }
    };
    match f(&db) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn create(db: &Db, name: &str, rotate: bool) -> Result<(), PeerTokenError> {
    let token = peer_token::create(db, name, rotate)?;
    println!("{token}");
    eprintln!(
        "已为 peer {name} 签发机器 token（明文只显示这一次）：写到对方节点 peers[].token_file 指向的文件，chmod 600{}",
        if rotate { "；旧 token 已立即失效" } else { "" }
    );
    Ok(())
}

fn list(db: &Db) -> Result<(), PeerTokenError> {
    let rows = peer_token::list(db)?;
    if rows.is_empty() {
        println!("没有签发过机器 token；`agora peer token create <name>` 为一个 peer 签发");
        return Ok(());
    }
    print_table(&rows);
    Ok(())
}

fn print_table(rows: &[PeerToken]) {
    let row = |a: &str, b: &str, c: &str, d: &str| println!("{a:<16} {b:<20} {c:<20} {d}");
    row("NAME", "CREATED", "LAST_USED", "STATE");
    for t in rows {
        let state = if t.revoked_at.is_some() {
            "revoked"
        } else {
            "active"
        };
        row(
            &t.name,
            &t.created_at,
            t.last_used_at.as_deref().unwrap_or("-"),
            state,
        );
    }
}

fn revoke(db: &Db, name: &str) -> Result<(), PeerTokenError> {
    peer_token::revoke(db, name)?;
    println!("已吊销 peer {name} 的机器 token");
    Ok(())
}
