//! WebSocket connections to the T3 server on the machine, authenticated with the proxy's token
//! and optional edge headers (such as Agent37's `X-Agent37-Key`, which the edge strips before
//! forwarding `Authorization: Bearer` to `t3 serve`).

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderMap;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type Upstream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// How often the proxy pings an open socket, and how long silence means it is dead: a stopped
/// machine's relay can hold a socket open without a word.
pub const PING: Duration = Duration::from_secs(10);
pub const SILENCE: Duration = Duration::from_secs(30);

/// Opens `target` (path and query) on the T3 server as a WebSocket, injecting `extra_headers`
/// (e.g. `X-Agent37-Key`) alongside `Authorization: Bearer <token>`.
pub async fn connect_with_headers(
    base_url: &str,
    token: &str,
    target: &str,
    extra_headers: &HeaderMap,
) -> Result<Upstream> {
    let ws_base = base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut req = format!("{ws_base}{target}").into_client_request()?;
    req.headers_mut()
        .insert("authorization", HeaderValue::from_str(&format!("Bearer {token}"))?);
    for (k, v) in extra_headers {
        let name = HeaderName::from_bytes(k.as_str().as_bytes())?;
        let val = HeaderValue::from_bytes(v.as_bytes())?;
        req.headers_mut().insert(name, val);
    }
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(req))
        .await
        .context("upstream connect timed out")??;
    Ok(ws)
}

/// Opens `target` without extra edge headers (used when talking to the local desktop T3 server).
pub async fn connect(base_url: &str, token: &str, target: &str) -> Result<Upstream> {
    connect_with_headers(base_url, token, target, &HeaderMap::new()).await
}

/// The RPC socket. `client_query` carries the desktop's client* parameters through, minus its
/// proxy ticket. Upstream sends whole frames: the proxy does not split or join parts.
pub fn rpc_target(client_query: &str) -> String {
    let client = super::query_without(client_query, &["wsTicket", "orchestrationProtocol", "frameParts"]);
    let separator = if client.is_empty() { "" } else { "&" };
    format!("/ws?orchestrationProtocol=2{separator}{client}")
}

/// Sends a few unary requests on a short-lived connection and waits for their results, in
/// request order: the success value or the server's failure message.
pub async fn call_many_with_headers(
    base_url: &str,
    token: &str,
    extra_headers: &HeaderMap,
    requests: Vec<(String, Value)>,
) -> Result<Vec<std::result::Result<Value, String>>> {
    let mut ws = connect_with_headers(base_url, token, &rpc_target(""), extra_headers).await?;
    let mut results = vec![Ok(Value::Null); requests.len()];
    let mut pending = std::collections::HashMap::new();
    for (i, (tag, payload)) in requests.into_iter().enumerate() {
        let id = format!("{}", i + 1);
        pending.insert(id.clone(), i);
        let msg = json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []});
        ws.send(Message::text(msg.to_string())).await?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !pending.is_empty() {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .context("timed out waiting for results")?;
        let Some(msg) = msg else { bail!("connection closed") };
        if let Message::Text(text) = msg? {
            for mut v in super::session::messages(&text)? {
                if v["_tag"] == "Exit" {
                    if let Some(i) = pending.remove(&super::session::id_key(&v["requestId"])) {
                        results[i] = match v["exit"]["_tag"].as_str() {
                            Some("Success") => Ok(v["exit"]["value"].take()),
                            _ => Err(failure_message(&v["exit"])),
                        };
                    }
                }
            }
        }
    }
    let _ = ws.close(None).await;
    Ok(results)
}

pub async fn call_many(
    base_url: &str,
    token: &str,
    requests: Vec<(String, Value)>,
) -> Result<Vec<std::result::Result<Value, String>>> {
    call_many_with_headers(base_url, token, &HeaderMap::new(), requests).await
}

/// One request, as a T3 client makes it: its value, or the server's own failure message.
pub async fn call_with_headers(
    base_url: &str,
    token: &str,
    extra_headers: &HeaderMap,
    tag: &str,
    payload: Value,
) -> Result<Value> {
    subscribe_until_with_headers(base_url, token, extra_headers, tag, payload, Duration::from_secs(60), |_| None::<()>)
        .await
        .map(|(_, value)| value)
}

pub async fn call(base_url: &str, token: &str, tag: &str, payload: Value) -> Result<Value> {
    call_with_headers(base_url, token, &HeaderMap::new(), tag, payload).await
}

/// Opens `tag` and hands every streamed item to `on_item` until it returns a result, or the
/// request ends: `(Some(result), Null)` or `(None, exit value)`. Acks each chunk, as T3's
/// clients do, so the server keeps sending.
pub async fn subscribe_until_with_headers<T>(
    base_url: &str,
    token: &str,
    extra_headers: &HeaderMap,
    tag: &str,
    payload: Value,
    timeout: Duration,
    mut on_item: impl FnMut(&Value) -> Option<T>,
) -> Result<(Option<T>, Value)> {
    let mut ws = connect_with_headers(base_url, token, &rpc_target(""), extra_headers).await?;
    let msg = json!({"_tag": "Request", "id": "1", "tag": tag, "payload": payload, "headers": []});
    ws.send(Message::text(msg.to_string())).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let outcome = loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .with_context(|| format!("{tag} timed out"))?;
        let Some(msg) = msg else { bail!("{tag}: connection closed") };
        let Message::Text(text) = msg? else { continue };
        let mut found = None;
        for mut v in super::session::messages(&text)? {
            match v["_tag"].as_str().unwrap_or_default() {
                "Chunk" => {
                    ws.send(Message::text(json!({"_tag": "Ack", "requestId": "1"}).to_string())).await?;
                    for item in v["values"].as_array().into_iter().flatten() {
                        if let Some(result) = on_item(item) {
                            found = Some((Some(result), Value::Null));
                            break;
                        }
                    }
                }
                "Exit" if v["exit"]["_tag"] == "Success" => found = Some((None, v["exit"]["value"].take())),
                "Exit" => bail!("{}", failure_message(&v["exit"])),
                _ => {}
            }
            if found.is_some() {
                break;
            }
        }
        if let Some(outcome) = found {
            break outcome;
        }
    };
    let _ = ws.close(None).await;
    Ok(outcome)
}

pub async fn subscribe_until<T>(
    base_url: &str,
    token: &str,
    tag: &str,
    payload: Value,
    timeout: Duration,
    on_item: impl FnMut(&Value) -> Option<T>,
) -> Result<(Option<T>, Value)> {
    subscribe_until_with_headers(base_url, token, &HeaderMap::new(), tag, payload, timeout, on_item).await
}

/// What a failed request's cause says: the typed error's message, else its tag or the defect.
fn failure_message(exit: &Value) -> String {
    let cause = &exit["cause"][0];
    let error = &cause["error"];
    [&error["message"], &error["detail"], &error["_tag"], &cause["defect"]]
        .into_iter()
        .find_map(Value::as_str)
        .unwrap_or("the request failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_target_keeps_the_client_parameters_only() {
        assert_eq!(rpc_target(""), "/ws?orchestrationProtocol=2");
        assert_eq!(
            rpc_target("wsTicket=t&clientOs=mac%20os&orchestrationProtocol=1&frameParts=1&clientSurface=desktop"),
            "/ws?orchestrationProtocol=2&clientOs=mac%20os&clientSurface=desktop"
        );
    }
}
