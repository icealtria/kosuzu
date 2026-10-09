//! LLM translation over any OpenAI-compatible `/chat/completions` endpoint.
//!
//! - one `base_url` covers OpenAI, DeepSeek, NVIDIA NIM, OpenRouter, Ollama,
//!   LM Studio: they all speak the same wire format, so no provider crate is
//!   needed — a POST and a streamed JSON read, same as the Google engine;
//! - the response is streamed (`"stream": true`, sse): the UI shows text as it
//!   arrives instead of waiting tens of seconds for a full response;
//! - no chunking: a chat model has the context for whole texts, and splitting
//!   would break its sense of the sentence;
//! - chat completions are slow by nature, hence the generous timeout; the call
//!   still runs on the worker thread like every other engine.

use std::io::BufRead;
use std::time::Duration;

/// one chat completion regularly takes tens of seconds
pub const TIMEOUT: Duration = Duration::from_secs(120);

pub struct LlmTranslator {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl LlmTranslator {
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .user_agent("kosuzu")
            .build()
            .expect("reqwest client");
        Self {
            client,
            // a trailing slash would double up inside the url below
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            model: model.trim().to_string(),
            api_key: api_key.trim().to_string(),
        }
    }

    /// Translate `text`, calling `on_chunk` with each piece of the answer as it
    /// streams in. Returns once the stream ends; a failure part-way through is
    /// an error after whatever was already emitted.
    pub fn translate(
        &self,
        text: &str,
        source: &str,
        target: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> anyhow::Result<()> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("nothing to translate");
        }
        if self.model.is_empty() {
            anyhow::bail!("no LLM model configured");
        }
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.3,
            "stream": true,
            "messages": [
                {
                    "role": "system",
                    "content": format!(
                        "You are a translation engine. {} \
                         Reply with the translation only: \
                         no explanations, no quotation marks, keep line breaks.",
                        direction(source, target)
                    ),
                },
                {"role": "user", "content": text},
            ],
        });
        let resp = self
            .client
            .post(completions_url(&self.base_url))
            // local servers (ollama, lm studio) ignore it; the key may stay empty
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()?;
        let status = resp.status();
        if !status.is_success() {
            // error pages are not always json: a wrong url gives an html body
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("HTTP {status}: {}", body_detail(&body));
        }
        // blocking response is a Read: sse arrives line by line
        let mut received = false;
        for line in std::io::BufReader::new(resp).lines() {
            let line = line?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(data) else {
                continue; // comments, keep-alives, malformed lines
            };
            if let Some(delta) = payload
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
            {
                if !delta.is_empty() {
                    received = true;
                    on_chunk(delta);
                }
            }
        }
        if !received {
            anyhow::bail!("the model returned nothing");
        }
        Ok(())
    }
}

/// accept both a root (`.../v1`) and a fully-pasted endpoint url ending in
/// `/chat/completions` — provider docs hand out the full one
fn completions_url(base_url: &str) -> String {
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else {
        format!("{base_url}/chat/completions")
    }
}

/// the tidy root to store in config: a pasted full endpoint url reduces to it,
/// trailing slashes go away; `translate` accepts either, this is for storage
pub fn normalize_base_url(base_url: &str) -> String {
    let root = base_url.trim().trim_end_matches('/');
    root.strip_suffix("/chat/completions").unwrap_or(root).to_string()
}

/// "Translate the user's message from ja into en."; auto drops the source half.
fn direction(source: &str, target: &str) -> String {
    if source == "auto" || source.is_empty() {
        format!("Translate the user's message into {target}.")
    } else {
        format!("Translate the user's message from {source} into {target}.")
    }
}

/// error bodies are json when the provider is polite and html when it is not;
/// pull out something readable either way
fn body_detail(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .map(|v| api_error(&v))
        .unwrap_or_else(|_| body.chars().take(200).collect())
}

fn api_error(payload: &serde_json::Value) -> String {
    payload
        .pointer("/error/message")
        .or_else(|| payload.pointer("/message"))
        .or_else(|| payload.pointer("/error"))
        .and_then(|v| v.as_str())
        .unwrap_or("no message")
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct Recorded {
        target: String, // path?query
        auth: String,
        body: String,
    }

    /// one-shot mock: replies with `response`, records the request it saw
    fn mock(response: &str) -> (String, Arc<Mutex<Option<Recorded>>>) {
        mock_reply("200 OK", response)
    }

    fn mock_reply(
        status: &str,
        response: &str,
    ) -> (String, Arc<Mutex<Option<Recorded>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(None));
        let reply = response.to_string();
        let status = status.to_string();
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let (head, body) = raw.split_once("\r\n\r\n").unwrap_or_default();
                let target = head
                    .lines()
                    .find(|l| l.starts_with("POST "))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let auth = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim())
                    .unwrap_or("")
                    .to_string();
                *seen2.lock().unwrap() =
                    Some(Recorded { target, auth, body: body.to_string() });
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes(),
                );
            }
        });
        (url, seen)
    }

    /// the sse wire format of `"stream": true`
    fn sse(chunks: &[&str]) -> String {
        let mut out = String::new();
        for chunk in chunks {
            let payload = serde_json::json!({"choices": [{"delta": {"content": chunk}}]});
            out.push_str(&format!("data: {payload}\n\n"));
        }
        out.push_str("data: [DONE]\n\n");
        out
    }

    #[test]
    fn streams_the_translation_chunk_by_chunk() {
        let (url, seen) = mock(&sse(&["Bon", "jour\n", "monde"]));
        let translator = LlmTranslator::new(&url, "gpt-test", "secret-key");
        let chunks = Rc::new(RefCell::new(Vec::new()));
        let sink = chunks.clone();
        translator
            .translate("hello world", "en", "fr", &mut |c| sink.borrow_mut().push(c.to_string()))
            .unwrap();
        assert_eq!(&*chunks.borrow(), &["Bon", "jour\n", "monde"]);

        let seen = seen.lock().unwrap().take().expect("request seen");
        assert_eq!(seen.target, "/chat/completions");
        assert_eq!(seen.auth, "Bearer secret-key");
        let body: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["stream"], true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("from en into fr"),
            "{}",
            messages[0]["content"]
        );
        assert_eq!(messages[1]["content"], "hello world");
    }

    #[test]
    fn auto_source_drops_the_source_half_of_the_prompt() {
        let (url, seen) = mock(&sse(&["hi"]));
        LlmTranslator::new(&url, "m", "")
            .translate("text", "auto", "de", &mut |_| {})
            .unwrap();
        let body = seen.lock().unwrap().take().unwrap().body;
        let messages: serde_json::Value = serde_json::from_str(&body).unwrap();
        let system = messages["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("into de"), "{system}");
        assert!(!system.contains("from"), "{system}");
    }

    #[test]
    fn pasted_full_endpoint_url_is_not_doubled() {
        // provider docs hand out https://host/v1/chat/completions; pasting the
        // whole thing must not become /chat/completions/chat/completions
        let (url, seen) = mock(&sse(&["ok"]));
        LlmTranslator::new(&format!("{url}/chat/completions"), "m", "")
            .translate("hi", "en", "fr", &mut |_| {})
            .unwrap();
        let seen = seen.lock().unwrap().take().unwrap();
        assert_eq!(seen.target, "/chat/completions");
    }

    #[test]
    fn html_error_page_is_reported_readably() {
        // what a doubled-up / wrong url answers with
        let (url, _seen) = mock_reply("404 Not Found", "<!doctype html><title>404</title>");
        let err = LlmTranslator::new(&url, "m", "")
            .translate("hi", "en", "zh-CN", &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("404"), "{err}");
        assert!(err.contains("<!doctype html>"), "{err}");
    }

    #[test]
    fn http_error_surfaces_the_api_message() {
        let (url, _seen) =
            mock_reply("401 Unauthorized", r#"{"error": {"message": "Invalid API key"}}"#);
        let err = LlmTranslator::new(&url, "gpt-test", "")
            .translate("hi", "en", "zh-CN", &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid API key"), "{err}");
        assert!(err.contains("401"), "{err}");
    }

    #[test]
    fn empty_stream_is_reported() {
        let (url, _seen) = mock("data: [DONE]\n\n");
        let err = LlmTranslator::new(&url, "m", "")
            .translate("hi", "en", "ja", &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("the model returned nothing"), "{err}");
    }

    #[test]
    fn missing_model_is_a_config_error_not_a_request() {
        let translator = LlmTranslator::new("http://127.0.0.1:1", "  ", "");
        let err = translator
            .translate("hi", "en", "fr", &mut |_| {})
            .unwrap_err()
            .to_string();
        assert!(err.contains("no LLM model configured"), "{err}");
    }

    #[test]
    fn trailing_slash_in_base_url_is_tolerated() {
        let (url, seen) = mock(&sse(&["ok"]));
        LlmTranslator::new(&format!("{url}/"), "m", "")
            .translate("hi", "en", "fr", &mut |_| {})
            .unwrap();
        let seen = seen.lock().unwrap().take().unwrap();
        assert_eq!(seen.target, "/chat/completions");
    }

    #[test]
    fn normalize_base_url_reduces_a_pasted_endpoint() {
        assert_eq!(
            normalize_base_url("https://integrate.api.nvidia.com/v1/chat/completions"),
            "https://integrate.api.nvidia.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://integrate.api.nvidia.com/v1"),
            "https://integrate.api.nvidia.com/v1"
        );
        assert_eq!(normalize_base_url("http://localhost:11434/v1/"), "http://localhost:11434/v1");
    }
}
