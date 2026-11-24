use agent2api_wasm_sdk::{self as sdk, json, Error, Result, Value};

pub fn account() -> Result<Value> {
    let account = sdk::account()?;
    if sdk::account_disabled(&account) {
        return Err(Error::http(403, "此账号已禁用"));
    }
    if !account["secret"].is_object() {
        return Err(Error::http(401, "请先添加账号"));
    }
    Ok(account)
}

pub fn classify(status: u16, code: Option<i64>, auth: bool, message: &str) -> Value {
    if auth {
        json!({"kind":"token_expired", "message":message})
    } else if matches!(status, 402 | 429) {
        json!({"kind":"quota_limited", "status":status, "message":message, "upstream_code":code})
    } else {
        json!({"kind":"fatal", "status":status, "message":message, "upstream_code":code})
    }
}

pub fn head(status: u16, mime: &str) -> Result<()> {
    sdk::host(
        "stream.head",
        json!({"status":status,"headers":[["content-type",mime]]}),
    )?;
    Ok(())
}
