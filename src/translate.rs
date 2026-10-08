//! Google free-endpoint translation client (`client=gtx`, no API key).
//!
//! - `CHUNK_LIMIT` = 4500 chars/request, split on line boundaries, results concatenated in order;
//! - three strategies rotate on 429/403/non-2xx/parse errors; the last winner is
//!   tried first next time; all failures are aggregated into one error.

use std::time::Duration;

pub const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
/// conservative per-request limit for the free endpoint
pub const CHUNK_LIMIT: usize = 4500;

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/126.0 Safari/537.36";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationResult {
    pub text: String,
    pub detected_source_lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    PostGtx,
    GetChromeEx,
    GetGtx,
}

impl Strategy {
    const ALL: [Strategy; 3] = [Self::PostGtx, Self::GetChromeEx, Self::GetGtx];

    fn name(self) -> &'static str {
        match self {
            Self::PostGtx => "post_gtx",
            Self::GetChromeEx => "get_chrome_ex",
            Self::GetGtx => "get_gtx",
        }
    }

    fn client(self) -> &'static str {
        match self {
            Self::PostGtx => "gtx",
            Self::GetChromeEx => "dict-chrome-ex",
            Self::GetGtx => "gtx",
        }
    }
}

/// split on line boundaries so each chunk is <= `limit` chars; concat == input.
pub fn split_text(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "limit must be positive");
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for line in text.split_inclusive('\n') {
        let mut seg: Vec<char> = line.chars().collect();
        while seg.len() > limit {
            if !current.is_empty() {
                chunks.push(current.iter().collect());
                current.clear();
            }
            chunks.push(seg[..limit].iter().collect());
            seg.drain(..limit);
        }
        if current.len() + seg.len() > limit {
            chunks.push(current.iter().collect());
            current = seg;
        } else {
            current.extend(seg);
        }
    }
    if !current.is_empty() {
        chunks.push(current.iter().collect());
    }
    chunks
}

pub struct GoogleTranslator {
    client: reqwest::blocking::Client,
    endpoint: String,
    /// index of the last successful strategy in `Strategy::ALL`
    best: Option<usize>,
}

impl GoogleTranslator {
    pub fn new(timeout: Duration) -> Self {
        Self::with_endpoint(ENDPOINT, timeout)
    }

    /// for tests: endpoint points at a local mock.
    pub fn with_endpoint(endpoint: &str, timeout: Duration) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client");
        Self {
            client,
            endpoint: endpoint.to_string(),
            best: None,
        }
    }

    /// name of the last successful strategy.
    pub fn active_strategy(&self) -> Option<&'static str> {
        self.best.map(|i| Strategy::ALL[i].name())
    }

    pub fn translate(
        &mut self,
        text: &str,
        source: &str,
        target: &str,
    ) -> anyhow::Result<TranslationResult> {
        let text = text.trim();
        if text.is_empty() {
            return Err(anyhow::anyhow!("nothing to translate"));
        }

        let mut detected: Option<String> = None;
        let mut parts: Vec<String> = Vec::new();
        for chunk in split_text(text, CHUNK_LIMIT) {
            let (seg, det) = self.request(&chunk, source, target)?;
            parts.push(seg);
            if detected.is_none() && !det.as_deref().unwrap_or("").is_empty() {
                detected = det;
            }
        }

        Ok(TranslationResult {
            text: parts.concat(),
            detected_source_lang: detected,
        })
    }

    fn request(
        &mut self,
        query: &str,
        source: &str,
        target: &str,
    ) -> anyhow::Result<(String, Option<String>)> {
        // round-robin starting from the last winner
        let mut errors: Vec<String> = Vec::new();
        for i in 0..Strategy::ALL.len() {
            let idx = (self.best.unwrap_or(0) + i) % Strategy::ALL.len();
            let strategy = Strategy::ALL[idx];
            let resp = match self.send(strategy, query, source, target) {
                Ok(r) => r,
                Err(e) => {
                    errors.push(format!("{}: {e}", strategy.name()));
                    continue;
                }
            };
            let status = resp.status();
            if status.as_u16() == 429 || status.as_u16() == 403 {
                // rate-limited: try the next strategy
                errors.push(format!("{}: HTTP {status}", strategy.name()));
                continue;
            }
            if !status.is_success() {
                errors.push(format!("{}: HTTP {status}", strategy.name()));
                continue;
            }
            let data: serde_json::Value = match resp.json() {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!("{}: {e}", strategy.name()));
                    continue;
                }
            };
            let Some(list) = data.as_array().filter(|a| !a.is_empty()) else {
                errors.push(format!("{}: unexpected response shape", strategy.name()));
                continue;
            };
            let Some(segs) = list[0].as_array() else {
                errors.push(format!("{}: unexpected response shape", strategy.name()));
                continue;
            };

            self.best = Some(idx);
            // [[seg, seg, ...], _, "detected", ...]; seg[0] is the translated segment
            let text = segs
                .iter()
                .filter_map(|seg| {
                    let arr = seg.as_array()?;
                    let s = arr.first()?.as_str()?;
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                })
                .collect::<String>();
            let detected = list.get(2).and_then(|v| v.as_str()).map(str::to_string);
            return Ok((text, detected));
        }

        Err(anyhow::anyhow!(
            "Google translate request failed: {}",
            errors.join("; ")
        ))
    }

    fn send(
        &self,
        strategy: Strategy,
        query: &str,
        source: &str,
        target: &str,
    ) -> reqwest::Result<reqwest::blocking::Response> {
        let common = [
            ("sl", source),
            ("tl", target),
            ("dt", "t"),
            ("q", query),
        ];
        match strategy {
            Strategy::PostGtx => {
                let mut form: Vec<(&str, &str)> = vec![("client", "gtx")];
                form.extend_from_slice(&common);
                self.client.post(&self.endpoint).form(&form).send()
            }
            _ => self
                .client
                .get(&self.endpoint)
                .query(&common)
                .query(&[("client", strategy.client())])
                .send(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct Recorded {
        method: String,
        target: String, // path?query
        body: String,
    }

    impl Recorded {
        /// percent-decoded params from the query or form body
        fn param(&self, key: &str) -> Option<String> {
            for source in [&self.target, &self.body] {
                for pair in source.split(['&', '?']) {
                    let Some((k, v)) = pair.split_once('=') else {
                        continue;
                    };
                    if k == key {
                        return Some(percent_decode(v));
                    }
                }
            }
            None
        }
    }

    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                b'%' if i + 2 < bytes.len() => {
                    if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    out.push(bytes[i]);
                    i += 1;
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// minimal one-request HTTP/1.1 mock; responder yields (status, body).
    fn spawn_server<F>(responder: F) -> (String, Arc<Mutex<Vec<Recorded>>>)
    where
        F: Fn(&Recorded) -> (u16, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let responder = &responder;
                // read until end of headers, then Content-Length bytes
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let header_end = loop {
                    match stream.read(&mut tmp) {
                        Ok(0) => break None,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break Some(p + 4);
                            }
                            if buf.len() > 1 << 20 {
                                break None;
                            }
                        }
                        Err(_) => break None,
                    }
                };
                let Some(header_end) = header_end else { continue };
                let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                let mut lines = head.lines();
                let req_line = lines.next().unwrap_or_default();
                let mut parts = req_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let target = parts.next().unwrap_or_default().to_string();
                let content_len: usize = lines
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                let mut body = buf[header_end..].to_vec();
                while body.len() < content_len {
                    match stream.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&tmp[..n]),
                    }
                }
                let rec = Recorded {
                    method,
                    target,
                    body: String::from_utf8_lossy(&body).into_owned(),
                };
                let (status, resp_body) = responder(&rec);
                log2.lock().unwrap().push(rec);
                let reason = if status == 200 { "OK" } else { "Too Many Requests" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                // Connection: close -> reqwest opens a new connection
            }
        });
        (format!("http://{addr}/translate_a/single"), log)
    }

    const OK_JSON: &str = r#"[[["你好","hello",null,null,10]],null,"en"]"#;

    #[test]
    fn empty_input_rejected() {
        // empty input returns before any HTTP, no mock needed
        let mut t = GoogleTranslator::new(Duration::from_secs(5));
        let err = t.translate("   \n ", "auto", "zh-CN").unwrap_err();
        assert_eq!(err.to_string(), "nothing to translate");
    }

    #[test]
    fn fallback_on_429_and_remember_winner() {
        // POST 429 -> dict-chrome-ex 200; the winner is remembered for next time
        let (url, log) = spawn_server(|rec| {
            if rec.method == "POST" {
                (429, "{}".into())
            } else if rec.param("client").as_deref() == Some("dict-chrome-ex") {
                (200, OK_JSON.into())
            } else {
                (429, "{}".into())
            }
        });
        let mut t = GoogleTranslator::with_endpoint(&url, Duration::from_secs(5));

        let r = t.translate("hello", "auto", "zh-CN").unwrap();
        assert_eq!(r.text, "你好");
        assert_eq!(r.detected_source_lang.as_deref(), Some("en"));
        assert_eq!(t.active_strategy(), Some("get_chrome_ex"));

        log.lock().unwrap().clear();
        t.translate("world", "auto", "zh-CN").unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1, "remembered strategy should hit once, no rotation");
        assert_eq!(log[0].method, "GET");
        assert_eq!(log[0].param("client").as_deref(), Some("dict-chrome-ex"));
    }

    #[test]
    fn all_strategies_fail_aggregates_errors() {
        let (url, _) = spawn_server(|_| (429, "{}".into()));
        let mut t = GoogleTranslator::with_endpoint(&url, Duration::from_secs(5));
        let err = t.translate("hello", "auto", "zh-CN").unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("Google translate request failed: "), "{msg}");
        for name in ["post_gtx", "get_chrome_ex", "get_gtx"] {
            assert!(msg.contains(name), "missing {name}: {msg}");
        }
    }

    #[test]
    fn bad_json_structure_falls_through() {
        let (url, log) = spawn_server(|_| (200, r#"{"nope":1}"#.into()));
        let mut t = GoogleTranslator::with_endpoint(&url, Duration::from_secs(5));
        let err = t.translate("hello", "auto", "zh-CN").unwrap_err();
        assert!(err.to_string().contains("unexpected response shape"));
        assert_eq!(log.lock().unwrap().len(), 3, "broken shape must exhaust all three strategies");
    }

    #[test]
    fn long_text_chunks_join_back_exactly() {
        // mixed newlines + CJK: chunk <= 4500 chars and round-trip
        let mut text = String::new();
        for i in 0..1000 {
            text.push_str(&format!("line {i} chinese test text with english\n"));
        }
        text.push_str(&"x".repeat(9000)); // one over-long line without newlines must be split
        let chunks = split_text(&text, CHUNK_LIMIT);
        assert!(chunks.len() > 3, "should split into multiple chunks: {}", chunks.len());
        for c in &chunks {
            assert!(c.chars().count() <= CHUNK_LIMIT, "chunk too big: {}", c.chars().count());
        }
        assert_eq!(chunks.concat(), text, "concat must restore the input");
    }

    #[test]
    fn split_short_text_single_chunk() {
        assert_eq!(split_text("hello", 10), vec!["hello"]);
        assert_eq!(split_text("", 10), vec![""]);
        // exactly at the limit: no split
        let exact = "a".repeat(CHUNK_LIMIT);
        assert_eq!(split_text(&exact, CHUNK_LIMIT), vec![exact]);
        // one char over the limit: split
        let over = "a".repeat(CHUNK_LIMIT + 1);
        let chunks = split_text(&over, CHUNK_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.concat(), over);
    }

    #[test]
    fn chunked_requests_stay_under_limit_and_concat() {
        // >4500 chars: multiple requests, each query <= 4500, results concatenated in order
        let (url, log) = spawn_server(|_| {
            (200, r#"[[["好","x",null,null,1]],null,"zh"]"#.into())
        });
        let mut t = GoogleTranslator::with_endpoint(&url, Duration::from_secs(5));
        let text = "multi-line test\n".repeat(800); // 10400 chars
        let r = t.translate(&text, "en", "zh-CN").unwrap();
        let log = log.lock().unwrap();
        assert!(log.len() >= 2, "should request in chunks: {}", log.len());
        for rec in log.iter() {
            let q = rec.param("q").unwrap();
            assert!(q.chars().count() <= CHUNK_LIMIT, "single request over limit: {}", q.chars().count());
        }
        let expect = "好".repeat(log.len());
        assert_eq!(r.text, expect);
    }

    /// live smoke test: may be 429-limited, ignored by default.
    #[test]
    #[ignore = "needs network and may be 429-limited by translate.googleapis.com"]
    fn live_translate_smoke() {
        let mut t = GoogleTranslator::new(Duration::from_secs(15));
        let r = t.translate("hello world", "auto", "zh-CN").unwrap();
        assert!(!r.text.is_empty());
        assert!(r.detected_source_lang.is_some());
    }
}
