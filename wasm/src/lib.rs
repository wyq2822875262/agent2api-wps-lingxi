mod chat;
mod common;
mod upstream;

use agent2api_wasm_sdk::{self as sdk, catalog, json, Error, Result, Value};
use std::collections::BTreeMap;
use upstream::{Failure, Wps};

sdk::export_provider!(handle);

const ID: &str = "wps-lingxi";
const LABEL: &str = "WPS 灵犀";
const LEDGER: &str = "wps-lingxi-daily";

fn view(secret: &Value) -> Value {
    let nickname = secret["nickname"].as_str().unwrap_or("");
    let name = secret["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| Some(nickname).filter(|s| !s.is_empty()))
        .unwrap_or(LABEL);
    json!({"name": name, "uid": secret["uid"].as_str(), "nickname": nickname})
}

fn identity(secret: &Value) -> Result<String> {
    let uid = secret["uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::http(401, "缺少已验证的 WPS 账号 ID"))?;
    Ok(format!("{ID}:{uid}"))
}

fn label(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}

fn number(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| *n >= 0)
}

fn methods() -> Value {
    json!({"instructions":"请在此处填写自己的 WPS 灵犀 wps_sid，不要粘贴完整 Cookie。", "methods":[{"methodIndex":0,"type":"api","label":"WPS wps_sid","prompts":[
        {"key":"wps_sid","type":"text","label":"wps_sid","secret":true,"optional":false}
    ]}]})
}

fn connect() -> Result<Wps> {
    Wps::new(
        common::account()?["secret"]["wps_sid"]
            .as_str()
            .unwrap_or(""),
    )
}

fn models() -> Result<Value> {
    let wps = connect()?;
    let env = wps.get(upstream::PLANS)?;
    let rows = env["data"]["models"]
        .as_array()
        .ok_or_else(|| Error::http(502, "WPS 灵犀模型目录格式无效"))?;
    let mut models = BTreeMap::new();
    for row in rows {
        let Some(key) = row["key"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 256)
        else {
            continue;
        };
        let name = row["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(key);
        let mut model = json!({"id":key,"object":"model","name":name,"owned_by":ID});
        for (upstream, gateway) in [
            ("max_context_tokens", "contextWindow"),
            ("max_new_tokens", "maxOutputTokens"),
        ] {
            if let Some(n) = number(&row[upstream]).filter(|n| *n > 0) {
                model[gateway] = json!(n);
            }
        }
        models.entry(key).or_insert(model);
    }
    if models.is_empty() {
        return Err(Error::http(502, "WPS 灵犀未返回可用模型，请检查账号权限"));
    }
    Ok(catalog::refreshed(models.into_values().collect()))
}

fn daily_task(wps: &Wps) -> Result<Option<Value>> {
    let env = wps.get(upstream::TASKS)?;
    let tasks = env["data"]["tasks"]
        .as_array()
        .ok_or_else(|| Error::http(502, "WPS 灵犀任务列表格式无效"))?;
    Ok(tasks
        .iter()
        .find(|task| task["task_key"] == "daily_check_in")
        .cloned())
}

fn report(outcome: &str, message: &str, task: &Value) -> Value {
    let streak = number(&task["extra"]["consecutive_days"]).and_then(|n| u32::try_from(n).ok());
    json!({"outcome":outcome, "message":message,"streak":streak})
}

fn daily() -> Result<Value> {
    let _guard = sdk::AccountLock::acquire()?;
    let wps = connect()?;
    let Some(task) = daily_task(&wps)? else {
        return Ok(report("inactive", "当前账号没有每日签到任务", &Value::Null));
    };
    if task["status"] == "claimed" {
        return Ok(report("done", "今日已签到", &task));
    }
    if !matches!(task["status"].as_str(), Some("incomplete" | "claimable")) {
        return Ok(report("inactive", "当前每日签到任务不可领取", &task));
    }
    let now = sdk::now();
    let day = chrono::DateTime::from_timestamp_millis(now.saturating_add(8 * 3600 * 1000))
        .filter(|_| now > 0)
        .ok_or_else(|| Error::new("宿主时钟不可用"))?
        .format("%Y-%m-%d")
        .to_string();
    let prior = sdk::host("ledger.get", json!({"kind":LEDGER}))?;
    let uncertain = "今日签到结果待确认，请在 WPS 灵犀检查；本插件不会重发领取";
    if prior["day"] == day {
        return Ok(if prior["state"] == "claimed" {
            report("done", "今日已签到", &task)
        } else {
            report("failed", uncertain, &task)
        });
    }
    sdk::host(
        "ledger.put",
        json!({"kind":LEDGER,"value":{"day":day,"state":"pending"}}),
    )?;
    let attempt = wps.json("POST", &format!("{}?date={day}", upstream::CLAIM), None);
    match attempt {
        Ok(env) if env["result"] == "ok" => {
            sdk::host(
                "ledger.put",
                json!({"kind":LEDGER,"value":{"day":day,"state":"claimed"}}),
            )?;
            let mut result = report("claimed", "今日签到成功", &env["data"]);
            result["credit"] =
                json!(number(&env["data"]["extra"]["today_reward"]).map(|n| format!("{n} 智点")));
            Ok(result)
        }
        Err(e) if e.status == Some(401) => Err(e),
        _ => {
            // A timed-out write may still have succeeded. A read is safe; a second write is not.
            if let Ok(Some(after)) = daily_task(&wps) {
                if after["status"] == "claimed" {
                    return Ok(report("done", "今日已签到", &after));
                }
            }
            Ok(report("failed", uncertain, &task))
        }
    }
}

fn metadata() -> Value {
    json!({"id":ID,"label":LABEL,"order":21,
        "capabilities":{"chat":true,"usage":true,"checkin":true,"model_refresh":true,
            "sse_model_rewrite":true,"supports_default_model":false},
        "account_features":{"usage":true,"identifier":["uid"]},
        "checkin":{"daily":true,"modes":["checkin"],"description":"领取 WPS 灵犀每日签到智点"},
        "default_models":[]})
}

fn normalize(params: &Value) -> Result<Value> {
    let source = &params["source"];
    if source["kind"] != "manual" {
        return Err(Error::unsupported());
    }
    let manual = &source["payload"];
    let fields = match manual.get("methodIndex") {
        Some(method) if method.as_u64() == Some(0) => manual
            .get("inputs")
            .filter(|v| v.is_object())
            .ok_or_else(|| Error::http(400, "WPS 灵犀账号表单格式无效"))?,
        Some(_) => return Err(Error::http(400, "WPS 灵犀凭证导入方式无效")),
        None => manual,
    };
    let raw = fields["wps_sid"]
        .as_str()
        .or_else(|| fields["token"].as_str())
        .ok_or_else(|| Error::http(400, "请填写 wps_sid"))?;
    let sid = upstream::cookie_value(raw).map_err(Failure::rpc)?;
    let env = Wps::new(sid)?.get(upstream::PROFILE)?;
    let user = &env["data"];
    let uid = user["id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| user["id"].as_u64().map(|n| n.to_string()))
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
        .ok_or_else(|| Error::http(502, "WPS 灵犀未返回有效的账号 ID"))?;
    let secret = json!({"wps_sid":sid,"uid":uid,"nickname":label(user["name"].as_str()),
        "name":label(params["name"].as_str().or_else(|| manual["name"].as_str()))});
    Ok(
        json!({"public":view(&secret),"identity":identity(&secret)?,"secret":secret,"desktop":false}),
    )
}

fn usage() -> Result<Value> {
    let env = connect()?.get(upstream::BALANCE)?;
    let data = &env["data"];
    let balance = number(
        data.get("total_balance")
            .filter(|v| !v.is_null())
            .unwrap_or(&data["balance"]),
    )
    .ok_or_else(|| Error::http(502, "WPS 灵犀未返回有效智点余额"))?;
    Ok(json!({"provider":ID,"plan":LABEL,"available":balance,"balance":balance,"unit":"智点"}))
}

fn handle(method: &str, params: Value) -> Result<Value> {
    match method {
        "init" => Ok(json!([metadata()])),
        "provider.info" => Ok(metadata()),
        "ping" => Ok(json!(true)),
        "admin.action" if params["action"] == "auth.methods" => Ok(methods()),
        "account.normalize" => normalize(&params),
        "account.view" => Ok(view(&params["secret"])),
        "account.identity" => Ok(json!(identity(&params["secret"])?)),
        "models.list" => catalog::cached(),
        "models.refresh" => models(),
        "usage.query" => usage(),
        "checkin.claim" => daily(),
        "conversation" => {
            chat::forward(connect()?, params)?;
            Ok(Value::Null)
        }
        "classify" => {
            let status = params["status"].as_u64().unwrap_or(502) as u16;
            let failure = upstream::http_failure(status, &params["body"]);
            Ok(common::classify(
                failure.0,
                None,
                failure.0 == 401,
                failure.1,
            ))
        }
        "is_error_response" => Ok(json!(params["status"].as_u64().unwrap_or(502) >= 400)),
        _ => Err(Error::unsupported()),
    }
}
