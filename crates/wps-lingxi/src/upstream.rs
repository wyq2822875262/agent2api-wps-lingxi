use agent2api_provider_protocol::WireProxy;
use agent2api_provider_sdk::{extensions::direct_proxy_override, PluginCtx, StoreProxy};
use reqwest::{header, Client, Method, Response, Url};
use serde_json::Value;
use std::time::Duration;

const BASE: &str = "https://lingxi.kdocs.cn";
pub const PROFILE: &str = "/api/public/v1/account/profile";
pub const BALANCE: &str = "/api/public/v1/credits/balance";
pub const TASKS: &str = "/api/public/v1/tasks";
pub const CLAIM: &str = "/api/public/v1/tasks/daily_check_in/claim";
pub const PLANS: &str = "/api/aioffice/v1/sessions/plans";
pub const SESSIONS: &str = "/api/aigc/v3/assistant/sessions";

pub struct Failure(pub u16, pub &'static str);

impl Failure {
    pub fn rpc(self) -> String {
        agent2api_provider_sdk::control_error(i32::from(self.0), self.1, None, None)
    }
}

pub fn invalid_response() -> Failure {
    Failure(502, "WPS 灵犀返回了无法识别的响应")
}

fn network(err: reqwest::Error) -> Failure {
    if err.is_timeout() {
        Failure(504, "WPS 灵犀请求超时")
    } else {
        Failure(502, "无法与 WPS 灵犀通信")
    }
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

fn base() -> Result<String, Failure> {
    // Release packs cannot override the credential destination.
    #[cfg(debug_assertions)]
    if let Some(raw) = std::env::var_os("WPS_LINGXI_TEST_BASE") {
        let invalid = || Failure(500, "WPS 灵犀测试端点仅允许 HTTP loopback");
        let url = Url::parse(raw.to_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid());
        }
        return Ok(url.origin().ascii_serialization());
    }
    Ok(BASE.into())
}

pub fn cookie_value(raw: &str) -> Result<&str, Failure> {
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

fn static_proxy(config: &Value) -> Result<&str, Failure> {
    let invalid = || Failure(503, "WPS 灵犀账号静态代理配置无效，请检查账号的代理设置");
    let config = config
        .get("config")
        .filter(|value| value.is_object())
        .unwrap_or(config);
    if !config.is_object() || config.get("error").is_some_and(|value| !value.is_null()) {
        return Err(invalid());
    }
    match config.get("source") {
        Some(value) if value == "direct" => return Ok("direct"),
        None => {}
        Some(value) if value == "custom" => {}
        // Pool/Clash/etc. need an actual endpoint from the gateway, not a guessed route.
        _ => return Err(invalid()),
    }
    ["url", "proxy", "address", "server"]
        .into_iter()
        .filter_map(|key| config.get(key).and_then(Value::as_str))
        .map(str::trim)
        .find(|url| !url.is_empty())
        // Only the explicit source may select direct; an invalid URL must not disable a proxy.
        .filter(|url| *url != "direct")
        .ok_or_else(invalid)
}

pub struct Wps {
    client: Client,
    cookie: header::HeaderValue,
    base: String,
}

impl Wps {
    pub fn new(
        ctx: &PluginCtx,
        sid: &str,
        explicit: Option<&WireProxy>,
        stored: Option<&StoreProxy>,
    ) -> Result<Self, Failure> {
        let invalid = || Failure(503, "WPS 灵犀代理地址无效，请检查协议、主机和端口");
        let proxy = if direct_proxy_override() {
            String::new()
        } else if let Some(proxy) = explicit {
            proxy.to_url()
        } else {
            match stored {
                Some(StoreProxy::Resolved { proxy }) => proxy.to_url(),
                // Explicit direct is Static on the wire, and must override the gateway default.
                Some(StoreProxy::Static { config }) => static_proxy(config)?.to_owned(),
                Some(StoreProxy::Failed { .. }) => {
                    return Err(Failure(
                        503,
                        "WPS 灵犀账号代理当前不可用，请检查该账号的代理设置",
                    ))
                }
                _ => ctx.env.proxy.clone(),
            }
        };
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(90));
        if !matches!(proxy.as_str(), "" | "direct") {
            let url = Url::parse(&proxy).map_err(|_| invalid())?;
            if !matches!(
                url.scheme(),
                "http" | "https" | "socks4" | "socks5" | "socks5h"
            ) || !url.has_host()
                || url.port() == Some(0)
                || url.path() != "/" && !url.path().is_empty()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid());
            }
            builder = builder.proxy(reqwest::Proxy::all(url.as_str()).map_err(|_| invalid())?);
        }
        let sid = cookie_value(sid)?;
        let mut cookie = header::HeaderValue::from_str(&format!("wps_sid={sid}"))
            .map_err(|_| Failure(400, "wps_sid 格式无效"))?;
        cookie.set_sensitive(true);
        Ok(Self {
            client: builder.build().map_err(network)?,
            cookie,
            base: base()?,
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        sse: bool,
    ) -> Result<Response, Failure> {
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header(header::COOKIE, self.cookie.clone())
            .header(
                header::ACCEPT,
                if sse {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            );
        if let Some(body) = body {
            req = req.json(body);
        }
        if sse {
            req = req
                .header("X-Cc-Version", "1")
                .header("X-Client-Type", "lingxi-web")
                .header("X-Cc-Entrance", "default");
        } else {
            req = req.timeout(Duration::from_secs(25));
        }
        req.send().await.map_err(network)
    }

    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, Failure> {
        let mut resp = self.send(method, path, body, false).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(http_failure(status, &Value::Null));
        }
        let envelope = read_json(&mut resp).await?;
        match envelope.get("result") {
            None | Some(Value::Null) => {}
            Some(v) if v == "ok" => {}
            _ => return Err(http_failure(status, &envelope)),
        }
        Ok(envelope)
    }

    pub async fn get(&self, path: &str) -> Result<Value, Failure> {
        self.json(Method::GET, path, None).await
    }

    pub async fn completions(&self, sid: &str, body: &Value) -> Result<Response, Failure> {
        if sid.is_empty()
            || sid.len() > 256
            || !sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(invalid_response());
        }
        let mut resp = self
            .send(
                Method::POST,
                &format!("{SESSIONS}/{sid}/completions"),
                Some(body),
                true,
            )
            .await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(http_failure(status, &Value::Null));
        }
        let mime = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/event-stream")
        {
            let envelope = read_json(&mut resp).await?;
            return Err(http_failure(status, &envelope));
        }
        Ok(resp)
    }
}

pub async fn chunk(resp: &mut Response) -> Result<Option<impl AsRef<[u8]>>, Failure> {
    resp.chunk().await.map_err(network)
}

async fn read_json(response: &mut Response) -> Result<Value, Failure> {
    let mut bytes = Vec::new();
    while let Some(part) = chunk(response).await? {
        if bytes.len() + part.as_ref().len() > 1024 * 1024 {
            return Err(invalid_response());
        }
        bytes.extend_from_slice(part.as_ref());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid_response())
}
