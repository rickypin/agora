//! `WS /api/events`：真实监听器上的订阅、合并突发、慢客户端 resync、跨站升级拒绝。

mod common;

use std::time::Duration;

use agora::events::{Event, EventBus};
use agora::status::{Source, Status};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use common::Fx;

/// 起一个真实监听器（oneshot 走不了 WS 升级），返回地址。
async fn listen(fx: &Fx) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = fx.app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr.to_string()
}

async fn connect(
    addr: &str,
    cookie: &str,
    origin: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut req = format!("ws://{addr}/api/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    req.headers_mut()
        .insert(header::ORIGIN, origin.parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn next_batch<S>(ws: &mut S) -> Vec<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("5 s 内应有事件")
        .unwrap()
        .unwrap();
    let Message::Text(text) = msg else {
        panic!("非文本帧: {msg:?}")
    };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn subscriber_gets_created_status_and_removed_in_coalesced_batches() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let mut ws = connect(&addr, &cookie, &format!("http://{addr}"))
        .await
        .expect("同源 + cookie 应升级成功");

    // 经 API 创建 → session_created。
    let client = create_via_api(&fx, &addr, &cookie).await;
    let created: Value = serde_json::from_slice(&client).unwrap();
    let gid = created["id"].as_str().unwrap().to_owned();
    let batch = next_batch(&mut ws).await;
    assert_eq!(batch[0]["type"], "session_created");
    assert_eq!(batch[0]["id"], gid);
    assert_eq!(batch[0]["session"]["id"], gid);

    // 突发合并：同一会话三次状态变化一帧只剩最后一条，别的事件原序保留。
    let bus: EventBus = fx.state.events.clone();
    for st in [Status::Starting, Status::Running, Status::Waiting] {
        bus.publish(Event::StatusChanged {
            id: gid.clone(),
            status: st,
            source: Source::Process,
            reason: None,
            alive: true,
        });
    }
    bus.publish(Event::SessionRemoved { id: gid.clone() });
    let batch = next_batch(&mut ws).await;
    let kinds: Vec<&str> = batch.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["status_changed", "session_removed"], "{batch:?}");
    assert_eq!(batch[0]["status"], "waiting");

    // ping → pong。
    ws.send(Message::Text("{\"type\":\"ping\"}".into()))
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(msg.to_text().unwrap(), "{\"type\":\"pong\"}");
}

/// 不引入 HTTP 客户端 crate：创建会话走 oneshot（同一个 AppState，总线共享）。
async fn create_via_api(fx: &Fx, _addr: &str, cookie: &str) -> Vec<u8> {
    let resp = fx
        .app()
        .oneshot(
            Request::post("/api/sessions")
                .header(header::HOST, common::HOST)
                .header(header::ORIGIN, format!("http://{}", common::HOST))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"display_name":"e","agent_type":"shell","working_directory":"/tmp","command":"sleep 1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    use http_body_util::BodyExt;
    resp.into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn cross_origin_upgrade_is_refused_and_no_cookie_is_401() {
    let fx = Fx::new();
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let Err(err) = connect(&addr, &cookie, "http://evil.example").await else {
        panic!("跨站升级必须失败")
    };
    assert!(
        matches!(err, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == StatusCode::FORBIDDEN),
        "{err:?}"
    );
    let Err(err) = connect(&addr, "agora_session=bogus", &format!("http://{addr}")).await else {
        panic!("无效 cookie 必须失败")
    };
    assert!(
        matches!(err, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == StatusCode::UNAUTHORIZED),
        "{err:?}"
    );
}

#[tokio::test]
async fn slow_subscriber_is_told_to_resync_instead_of_getting_a_backlog() {
    // 断流 / 落后 → 服务端只发 resync，客户端拉全量对齐（api.md 消费纪律）。
    let mut fx = Fx::new();
    fx.state.events = EventBus::with_capacity(2);
    let cookie = fx.cookie();
    let addr = listen(&fx).await;
    let mut ws = connect(&addr, &cookie, &format!("http://{addr}"))
        .await
        .unwrap();
    // 先让服务端的订阅建立起来（升级完成后 handler 才 subscribe）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bus = fx.state.events.clone();
    // 把 tokio 的 worker 让给 ws 任务之前一次性灌满：容量 2、发 10 条，必然落后。
    for i in 0..10 {
        bus.publish(Event::SessionRemoved {
            id: format!("n:{i}"),
        });
    }
    let batch = next_batch(&mut ws).await;
    let has_resync = batch.iter().any(|e| e["type"] == "resync");
    let removed = batch
        .iter()
        .filter(|e| e["type"] == "session_removed")
        .count();
    assert!(has_resync, "{batch:?}");
    assert!(removed < 10, "落后的事件不该被补齐: {batch:?}");
}
