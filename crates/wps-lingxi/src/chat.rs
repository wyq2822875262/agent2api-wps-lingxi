use crate::upstream::{self, Failure, Wps, SESSIONS};
use agent2api_provider_protocol::{ConversationRequest, StreamHead, UpstreamKind};
use agent2api_provider_sdk::ConversationSink;
use reqwest::Method;
use serde_json::{json, Value};

const MAX_FRAME: usize = 256 * 1024;
const MAX_CONTENT: usize = 4 * 1024 * 1024;

fn payload(req: &ConversationRequest) -> Result<(&str, Value), Failure> {
    let bad = || Failure(400, "WPS 灵犀仅支持以用户消息结尾的文本对话");
    let body = &req.body;
    let model = if req.wire_model.is_empty() {
        body["model"].as_str().unwrap_or("")
    } else {
        &req.wire_model
    };
    if model.trim().is_empty() || model.len() > 256 {
        return Err(Failure(400, "请选择一个 WPS 灵犀模型"));
    }
    if body
        .get("tools")
        .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        || body
            .get("functions")
            .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        || body
            .get("n")
            .is_some_and(|v| !v.is_null() && v.as_u64() != Some(1))
    {
        return Err(Failure(400, "WPS 灵犀不支持工具调用或一次生成多个候选"));
    }
    let messages = body["messages"]
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(bad)?;
    if messages.last().and_then(|m| m["role"].as_str()) != Some("user") {
        return Err(bad());
    }
    let mut prompt = String::new();
    for message in messages {
        if !message["tool_calls"].is_null() || !message["function_call"].is_null() {
            return Err(bad());
        }
        let role = match message["role"].as_str() {
            Some("system" | "developer") => "System: ",
            Some("assistant") => "Assistant: ",
            Some("user") => "User: ",
            _ => return Err(bad()),
        };
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        if messages.len() > 1 {
            prompt.push_str(role);
        }
        let content = &message["content"];
        if let Some(s) = content.as_str().filter(|s| !s.trim().is_empty()) {
            prompt.push_str(s);
        } else if let Some(parts) = content.as_array().filter(|parts| !parts.is_empty()) {
            for (i, part) in parts.iter().enumerate() {
                if part["type"] != "text" {
                    return Err(bad());
                }
                if i > 0 {
                    prompt.push('\n');
                }
                prompt.push_str(
                    part["text"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(bad)?,
                );
            }
        } else {
            return Err(bad());
        }
        if prompt.len() > 48 * 1024 {
            return Err(Failure(400, "WPS 灵犀文本对话最多支持 48 KiB 输入"));
        }
    }
    let mut body = json!({"model":model,"question":prompt,"user_input":{"text":prompt}});
    let effort = match req.body["reasoning_effort"].as_str() {
        Some("low") => Some("0"),
        Some("medium" | "high") => Some("1"),
        Some("max") => Some("2"),
        _ => None,
    };
    if let Some(effort) = effort {
        body["reasoning_effort"] = json!(effort);
    }
    Ok((model, body))
}

enum Event {
    Content(String),
    Reasoning(String),
    TextEnd,
    End,
}

#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    event: String,
    data: Vec<u8>,
}

impl Sse {
    fn bytes(&mut self, bytes: &[u8]) -> Result<Vec<Event>, Failure> {
        let mut events = Vec::new();
        for part in bytes.split_inclusive(|b| *b == b'\n') {
            if self.line.len() + self.data.len() + part.len() > MAX_FRAME {
                return Err(upstream::invalid_response());
            }
            self.line.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                self.line(&mut events)?;
                if matches!(events.last(), Some(Event::End)) {
                    break;
                }
            }
        }
        Ok(events)
    }

    fn line(&mut self, events: &mut Vec<Event>) -> Result<(), Failure> {
        let line = self.line.strip_suffix(b"\n").unwrap_or(&self.line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(value) = line.strip_prefix(b"event:") {
            let event = std::str::from_utf8(value)
                .map_err(|_| upstream::invalid_response())?
                .trim()
                .to_owned();
            if !self.data.is_empty() || !self.event.is_empty() {
                self.flush(events)?;
            }
            self.event = event;
        } else if let Some(value) = line.strip_prefix(b"data:") {
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data
                .extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
        } else if line.is_empty() {
            self.flush(events)?;
        }
        self.line.clear();
        Ok(())
    }

    fn flush(&mut self, events: &mut Vec<Event>) -> Result<(), Failure> {
        let event = std::mem::take(&mut self.event);
        let raw = std::mem::take(&mut self.data);
        if raw == b"[DONE]" {
            events.push(Event::End);
            return Ok(());
        }
        let value: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).map_err(|_| upstream::invalid_response())?
        };
        let kind = value["type"].as_str().unwrap_or(&event);
        let data = &value["data"];
        let text = data
            .as_str()
            .or_else(|| data["text"].as_str())
            .or_else(|| value.as_str())
            .unwrap_or("");
        match kind {
            "error" => {
                let error = upstream::http_failure(200, if data.is_null() { &value } else { data });
                return Err(if error.0 == 502 {
                    Failure(502, "WPS 灵犀未能完成本次对话")
                } else {
                    error
                });
            }
            "text" | "ppt_outline_text" if !text.is_empty() => {
                events.push(Event::Content(text.into()))
            }
            "reasoning" if !text.is_empty() => events.push(Event::Reasoning(text.into())),
            "end" | "text_end" => events.push(Event::TextEnd),
            "finish" => events.push(Event::End),
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<Vec<Event>, Failure> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.line(&mut events)?;
        }
        self.flush(&mut events)?;
        Ok(events)
    }
}

async fn head(sink: &mut ConversationSink<'_>, mime: &str) -> Result<(), String> {
    sink.head(StreamHead {
        status: 200,
        headers: [("content-type".into(), mime.into())].into(),
        upstream: UpstreamKind::Chat,
    })
    .await
}

async fn sse(sink: &mut ConversationSink<'_>, value: Value) -> Result<(), String> {
    sink.send(format!("data: {value}\n\n").as_bytes()).await
}

pub async fn run(
    wps: Wps,
    req: ConversationRequest,
    mut sink: ConversationSink<'_>,
) -> Result<(), String> {
    let cancel = sink.cancellation();
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("WPS 灵犀请求已取消".into()),
        result = tokio::time::timeout(std::time::Duration::from_secs(300), forward(wps, req, &mut sink)) => {
            result.unwrap_or_else(|_| Err(Failure(504, "WPS 灵犀对话超时").rpc()))
        }
    }
}

async fn forward(
    wps: Wps,
    req: ConversationRequest,
    sink: &mut ConversationSink<'_>,
) -> Result<(), String> {
    let (model, sent) = payload(&req).map_err(Failure::rpc)?;
    let shown_model = req.body["model"].as_str().unwrap_or(model);
    let created = chrono::Utc::now().timestamp();
    let id = format!("chatcmpl-wps-{:016x}", rand::random::<u64>());
    let frame = |delta: Value, finish: Value| {
        json!({
            "id":id,"object":"chat.completion.chunk","created":created,"model":shown_model,
            "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
        })
    };
    let session = wps
        .json(Method::POST, SESSIONS, Some(&json!({})))
        .await
        .map_err(Failure::rpc)?;
    let sid = session["data"]["session_id"]
        .as_str()
        .ok_or_else(|| upstream::invalid_response().rpc())?;
    let mut response = wps.completions(sid, &sent).await.map_err(Failure::rpc)?;
    let mut decoder = Sse::default();
    let (mut content, mut reasoning) = (String::new(), String::new());
    let mut sent_head = false;
    let mut text_end = false;
    let mut has_content = false;
    let mut size = 0;
    let mut output = 0;
    'read: loop {
        let next = upstream::chunk(&mut response).await.map_err(Failure::rpc)?;
        let events = match next {
            Some(ref chunk) => {
                size += chunk.as_ref().len();
                if size > 16 * 1024 * 1024 {
                    return Err(upstream::invalid_response().rpc());
                }
                decoder.bytes(chunk.as_ref())
            }
            None => decoder.finish(),
        }
        .map_err(Failure::rpc)?;
        for event in events {
            let (key, text, buffer) = match event {
                Event::End => break 'read,
                Event::TextEnd => {
                    text_end = true;
                    continue;
                }
                Event::Content(text) => {
                    has_content = true;
                    ("content", text, &mut content)
                }
                Event::Reasoning(text) => ("reasoning_content", text, &mut reasoning),
            };
            output += text.len();
            if output > MAX_CONTENT {
                return Err(upstream::invalid_response().rpc());
            }
            if req.stream {
                if !sent_head {
                    head(sink, "text/event-stream").await?;
                    sent_head = true;
                    sse(sink, frame(json!({"role":"assistant"}), Value::Null)).await?;
                }
                sse(sink, frame(json!({(key):text}), Value::Null)).await?;
            } else {
                buffer.push_str(&text);
            }
        }
        if next.is_none() {
            if text_end {
                break;
            }
            return Err(Failure(502, "WPS 灵犀流式回复提前中断").rpc());
        }
    }
    if !has_content {
        return Err(Failure(502, "WPS 灵犀未返回文本回复").rpc());
    }
    if req.stream {
        sse(sink, frame(json!({}), json!("stop"))).await?;
        sink.send(b"data: [DONE]\n\n").await
    } else {
        let mut message = json!({"role":"assistant","content":content});
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        let body = json!({"id":id,"object":"chat.completion","created":created,"model":shown_model,
            "choices":[{"index":0,"message":message,"finish_reason":"stop"}]})
        .to_string();
        head(sink, "application/json").await?;
        for chunk in body.as_bytes().chunks(16 * 1024) {
            sink.send(chunk).await?;
        }
        Ok(())
    }
}
