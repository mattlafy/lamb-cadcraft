//! The assistant's model client (TS `packages/llm-adapter/src/openAiCompatible.ts`
//! `OpenAiCompatibleProvider`): one non-streaming chat completion and the served-model list from
//! an OpenAI-compatible server, blocking, over HTTP/1.1 on `std::net` — `http://` only, no TLS.
//!
//! The base URL carries the `/v1` suffix ([`DEFAULT_BASE_URL`] is a local Ollama).
//! [`complete`] posts `{model, messages, stream: false}` to `{base}/chat/completions` and returns
//! `choices[0].message.content`; [`list_models`] gets `{base}/models` and returns every
//! `data[].id`, in server order. [`ask_fn`] wraps [`complete`] as the turn loop's
//! [`AskFn`](crate::assistant::AskFn).
//!
//! Every failure is an `Err(String)`: an unreachable server, a non-2xx status (with the body's
//! first 300 characters), an unexpected body, a non-`http://` URL, or a read that outlasts
//! [`ProviderConfig::read_timeout`] (no deadline when `None`). Connecting waits at most 5 s.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Value, json};

use crate::assistant::Message;

/// A local Ollama's OpenAI-compatible endpoint.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SNIPPET: usize = 300;

/// Which server and model [`complete`] talks to.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    /// Base URL including `/v1`, e.g. [`DEFAULT_BASE_URL`].
    pub base_url: String,
    pub model: String,
    /// How long to wait for the reply; `None` waits as long as the server takes.
    pub read_timeout: Option<Duration>,
}

/// One chat completion: the messages in, the reply text out.
pub fn complete(cfg: &ProviderConfig, messages: &[Message]) -> Result<String, String> {
    let url = join(&cfg.base_url, "chat/completions");
    let body = json!({ "model": cfg.model, "messages": messages, "stream": false }).to_string();
    let reply = request(&cfg.base_url, &url, "POST", Some(&body), cfg.read_timeout)?;
    let v = parse_json(&url, &reply)?;
    v["choices"][0]["message"]["content"].as_str().map(str::to_owned).ok_or_else(|| unexpected(&url, &reply))
}

/// The ids of every model the server lists, in its order.
pub fn list_models(base_url: &str) -> Result<Vec<String>, String> {
    let url = join(base_url, "models");
    let reply = request(base_url, &url, "GET", None, None)?;
    let v = parse_json(&url, &reply)?;
    let data = v["data"].as_array().ok_or_else(|| unexpected(&url, &reply))?;
    data.iter().map(|m| m["id"].as_str().map(str::to_owned).ok_or_else(|| unexpected(&url, &reply))).collect()
}

/// [`complete`] as the turn loop's model call.
pub fn ask_fn(cfg: ProviderConfig) -> impl FnMut(&[Message]) -> Result<String, String> {
    move |m: &[Message]| complete(&cfg, m)
}

fn join(base: &str, tail: &str) -> String {
    format!("{}/{tail}", base.trim_end_matches('/'))
}

fn snippet(s: &str) -> String {
    s.chars().take(SNIPPET).collect()
}

fn unexpected(url: &str, body: &str) -> String {
    format!("{url}: unexpected response: {}", snippet(body))
}

fn parse_json(url: &str, body: &str) -> Result<Value, String> {
    serde_json::from_str(body).map_err(|_| unexpected(url, body))
}

/// `http://host[:port]/path` → (`host:port` to connect to, `Host` header value, path).
fn split_url(url: &str) -> Result<(String, String, String), String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| format!("only http:// model servers are supported: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("no host in model server URL: {url}"));
    }
    let has_port = match authority.rfind(':') {
        Some(i) => !authority[i..].contains(']'),
        None => false,
    };
    let addr = if has_port { authority.to_owned() } else { format!("{authority}:80") };
    Ok((addr, authority.to_owned(), path.to_owned()))
}

fn unreachable_err(base: &str, e: &std::io::Error) -> String {
    format!("Cannot reach a model server at {base} ({e}). Is Ollama — or another OpenAI-compatible server — running there?")
}

fn connect(base: &str, addr: &str) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map_err(|e| unreachable_err(base, &e))?.collect();
    let mut last = std::io::Error::new(ErrorKind::NotFound, "no address");
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(unreachable_err(base, &last))
}

/// Sends one request and returns the 2xx body as text.
fn request(base: &str, url: &str, method: &str, body: Option<&str>, read_timeout: Option<Duration>) -> Result<String, String> {
    let (addr, host, path) = split_url(url)?;
    let mut stream = connect(base, &addr)?;
    stream.set_read_timeout(read_timeout).map_err(|e| format!("{url}: {e}"))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n");
    if let Some(b) = body {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body.unwrap_or_default().as_bytes());
    stream.write_all(&out).map_err(|e| unreachable_err(base, &e))?;
    let mut raw = Vec::new();
    if let Err(e) = stream.read_to_end(&mut raw) {
        return Err(match (e.kind(), read_timeout) {
            (ErrorKind::WouldBlock | ErrorKind::TimedOut, Some(t)) => format!("{url}: no reply within {} s", t.as_secs_f64()),
            _ => format!("{url}: {e}"),
        });
    }
    let (status, text) = parse_response(&raw).ok_or_else(|| unexpected(url, &String::from_utf8_lossy(&raw)))?;
    if !(200..300).contains(&status) {
        return Err(format!("{url} returned {status}: {}", snippet(&text)));
    }
    Ok(text)
}

/// A whole HTTP/1.x response → (status, decoded body).
fn parse_response(raw: &[u8]) -> Option<(u16, String)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let rest = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let (mut chunked, mut length) = (false, None);
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if k == "content-length" {
            length = v.parse::<usize>().ok();
        }
    }
    let body = if chunked {
        dechunk(rest)?
    } else if let Some(n) = length {
        rest.get(..n)?.to_vec()
    } else {
        rest.to_vec()
    };
    Some((status, String::from_utf8_lossy(&body).into_owned()))
}

fn dechunk(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&rest[..eol]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(rest.get(..size)?);
        rest = rest.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    /// Reads one request (head + Content-Length body) from `s`.
    fn read_request(s: &mut TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                if buf.len() >= i + 4 + len {
                    break;
                }
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// A one-shot server: answers one request with `response` (or holds it `silent_ms` unanswered),
    /// sending the recorded request back. Returns `(base_url, recorded)`.
    fn stub(response: String, silent_ms: u64) -> std::io::Result<(String, mpsc::Receiver<String>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            let req = read_request(&mut s).unwrap_or_default();
            let _ = tx.send(req);
            if silent_ms > 0 {
                thread::sleep(Duration::from_millis(silent_ms));
                return;
            }
            let _ = s.write_all(response.as_bytes());
        });
        Ok((base, rx))
    }

    fn ok_json(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    fn cfg(base: &str) -> ProviderConfig {
        ProviderConfig { base_url: base.to_owned(), model: "llama3".to_owned(), read_timeout: None }
    }

    fn msgs() -> Vec<Message> {
        vec![Message::new("system", "be brief"), Message::new("user", "hi")]
    }

    const REPLY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"Hello there"}}]}"#;

    #[test]
    fn a_completion_posts_the_messages_and_returns_the_content() -> R {
        let (base, rx) = stub(ok_json(REPLY), 0)?;
        assert_eq!(complete(&cfg(&base), &msgs())?, "Hello there");
        let req = rx.recv()?;
        assert!(req.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Content-Type: application/json") && req.contains("Connection: close"), "{req}");
        let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).ok_or("no body")?)?;
        assert_eq!(
            body,
            json!({ "model": "llama3", "messages": [{"role":"system","content":"be brief"},{"role":"user","content":"hi"}], "stream": false })
        );
        Ok(())
    }

    #[test]
    fn a_chunked_body_parses_the_same() -> R {
        let (a, b) = REPLY.split_at(20);
        let resp = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n", a.len(), b.len());
        let (base, _rx) = stub(resp, 0)?;
        assert_eq!(complete(&cfg(&base), &msgs())?, "Hello there");
        Ok(())
    }

    #[test]
    fn list_models_returns_ids_in_server_order() -> R {
        let (base, rx) = stub(ok_json(r#"{"object":"list","data":[{"id":"qwen2.5"},{"id":"llama3"},{"id":"mistral"}]}"#), 0)?;
        assert_eq!(list_models(&base)?, vec!["qwen2.5", "llama3", "mistral"]);
        assert!(rx.recv()?.starts_with("GET /v1/models HTTP/1.1\r\n"));
        Ok(())
    }

    #[test]
    fn a_server_error_names_the_status() -> R {
        let (base, _rx) = stub("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 13\r\n\r\nmodel crashed".to_owned(), 0)?;
        let e = complete(&cfg(&base), &msgs()).err().ok_or("expected an error")?;
        assert!(e.contains("/v1/chat/completions returned 500: model crashed"), "{e}");
        Ok(())
    }

    #[test]
    fn a_missing_field_is_an_unexpected_response() -> R {
        let (base, _rx) = stub(ok_json(r#"{"choices":[]}"#), 0)?;
        let e = complete(&cfg(&base), &msgs()).err().ok_or("expected an error")?;
        assert!(e.contains(": unexpected response: {\"choices\":[]}"), "{e}");
        Ok(())
    }

    #[test]
    fn a_closed_port_cannot_be_reached() -> R {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let e = list_models(&format!("http://127.0.0.1:{port}/v1")).err().ok_or("expected an error")?;
        assert!(e.starts_with(&format!("Cannot reach a model server at http://127.0.0.1:{port}/v1 (")), "{e}");
        Ok(())
    }

    #[test]
    fn https_is_refused_without_connecting() -> R {
        let e = complete(&cfg("https://api.example.com/v1"), &msgs()).err().ok_or("expected an error")?;
        assert_eq!(e, "only http:// model servers are supported: https://api.example.com/v1/chat/completions");
        Ok(())
    }

    #[test]
    fn a_silent_server_times_out() -> R {
        let (base, _rx) = stub(String::new(), 2000)?;
        let c = ProviderConfig { read_timeout: Some(Duration::from_millis(200)), ..cfg(&base) };
        let e = complete(&c, &msgs()).err().ok_or("expected an error")?;
        assert!(e.ends_with("/v1/chat/completions: no reply within 0.2 s"), "{e}");
        Ok(())
    }

    #[test]
    fn urls_split_into_address_host_and_path() -> R {
        assert_eq!(split_url("http://localhost:11434/v1/models")?, ("localhost:11434".into(), "localhost:11434".into(), "/v1/models".into()));
        assert_eq!(split_url("http://example.com")?, ("example.com:80".into(), "example.com".into(), "/".into()));
        assert_eq!(split_url("http://[::1]/v1")?, ("[::1]:80".into(), "[::1]".into(), "/v1".into()));
        Ok(())
    }

    #[test]
    fn run_turn_through_ask_fn_applies_a_script() -> R {
        use crate::assistant::{TurnOutcome, run_turn};
        use crate::commands::install_app;
        use cadcraft_engine::Session;

        install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        s.execute("lambproject", &json!({}))?;
        let content = "Here:\n```json\n{\"steps\":[{\"command\":\"line\",\"params\":{\"points\":[[0,0],[1000,0]]}}]}\n```";
        let reply = json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] }).to_string();
        let (base, _rx) = stub(ok_json(&reply), 0)?;
        let mut ask = ask_fn(cfg(&base));
        let out = run_turn(&mut s, "draw a line", &mut ask)?;
        assert!(matches!(out, TurnOutcome::Applied { attempts: 1, .. }), "{out:?}");
        Ok(())
    }
}
