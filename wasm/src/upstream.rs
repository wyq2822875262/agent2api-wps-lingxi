use agent2api_wasm_sdk::{
    http::{self, Response},
    json, Error, Result, Value,
};

const BASE: &str = "https://lingxi.kdocs.cn";
pub const PROFILE: &str = "/api/public/v1/account/profile";
pub const BALANCE: &str = "/api/public/v1/credits/balance";
pub const TASKS: &str = "/api/public/v1/tasks";
pub const CLAIM: &str = "/api/public/v1/tasks/daily_check_in/claim";
pub const PLANS: &str = "/api/aioffice/v1/sessions/plans";
pub const SESSIONS: &str = "/api/aigc/v3/assistant/sessions";

pub struct Failure(pub u16, pub &'static str);

impl Failure {
    pub fn rpc(self) -> Error {
        Error::http(self.0, self.1)
    }
}

pub fn invalid_response() -> Failure {
    Failure(502, "WPS 灵犀返回了无法识别的响应")
}

pub fn http_failure(status: u16, body: &Value) -> Failure {
    let code = body["result"].as_str().or_else(|| body["code"].as_str());
    let details = || {
        [
            body.as_str(),
            body["message"].as_str(),
            body["msg"].as_str(),
            body["hint"].as_str(),
            body["text"].as_str(),
        ]
        .into_iter()
        .flatten()
    };
    if status == 401
        || matches!(code, Some("UserUnauthorized" | "not_login"))
        || details().any(|s| s.contains("登录失效") || s.contains("请重新登录"))
    {
        return Failure(401, "WPS 登录已失效，请重新导入 wps_sid");
    }
    if status == 402
        || code == Some("InsufficientCredits")
        || details()
            .any(|s| s.contains("智点不足") || s.contains("智点余额不足") || s.contains("积分不足"))
    {
        return Failure(402, "WPS 灵犀智点不足");
    }
    match status {
        403 => Failure(403, "WPS 灵犀拒绝访问，请检查账号权限"),
        429 => Failure(429, "WPS 灵犀请求过于频繁"),
        400..=499 => Failure(status, "WPS 灵犀拒绝了请求"),
        503 => Failure(503, "WPS 灵犀暂不可用，请检查网络和代理设置"),
        504 => Failure(504, "WPS 灵犀请求超时"),
        _ => invalid_response(),
    }
}

pub fn cookie_value(raw: &str) -> std::result::Result<&str, Failure> {
    let raw = raw.trim();
    let sid = raw.strip_prefix("wps_sid=").unwrap_or(raw);
    // RFC 6265 cookie-octet subset; do not accept arbitrary Cookie headers.
    if sid.is_empty()
        || sid.len() > 4096
        || !sid
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
    {
        return Err(Failure(
            400,
            "请填写单个 wps_sid 值，不要导入完整 Cookie 请求头",
        ));
    }
    Ok(sid)
}

pub struct Wps {
    cookie: String,
}

impl Wps {
    pub fn new(sid: &str) -> Result<Self> {
        let sid = cookie_value(sid).map_err(Failure::rpc)?;
        Ok(Self {
            cookie: format!("wps_sid={sid}"),
        })
    }

    fn send(&self, method: &str, path: &str, body: Option<&Value>, sse: bool) -> Result<Response> {
        let mut headers = json!({"cookie":self.cookie,"accept":"application/json"});
        if body.is_some() {
            headers["content-type"] = json!("application/json");
        }
        let url = format!("{BASE}{path}");
        if sse {
            headers["accept"] = json!("text/event-stream");
            headers["x-cc-version"] = json!("1");
            headers["x-client-type"] = json!("lingxi-web");
            headers["x-cc-entrance"] = json!("default");
            http::open("POST", &url, headers, body)
        } else {
            http::open_timeout(method, &url, headers, body, 25_000)
        }
    }

    pub fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let response = self.send(method, path, body, false)?;
        let status = response.status;
        if !(200..300).contains(&status) {
            return Err(http_failure(status, &Value::Null).rpc());
        }
        let envelope = read_json(response)?;
        if !envelope.is_object() || (!envelope["result"].is_null() && envelope["result"] != "ok") {
            return Err(http_failure(status, &envelope).rpc());
        }
        Ok(envelope)
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.json("GET", path, None)
    }

    pub fn completions(&self, sid: &str, body: &Value) -> Result<Response> {
        if sid.is_empty()
            || sid.len() > 256
            || !sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid_response().rpc());
        }
        let response = self.send(
            "POST",
            &format!("{SESSIONS}/{sid}/completions"),
            Some(body),
            true,
        )?;
        let status = response.status;
        if !(200..300).contains(&status) {
            return Err(http_failure(status, &Value::Null).rpc());
        }
        let sse = response.headers_raw.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case("content-type")
                && value
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("text/event-stream")
        });
        if !sse {
            return Err(http_failure(status, &read_json(response)?).rpc());
        }
        Ok(response)
    }
}

fn read_json(response: Response) -> Result<Value> {
    serde_json::from_slice(&response.bytes(1024 * 1024)?).map_err(|_| invalid_response().rpc())
}
