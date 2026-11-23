mod chat;
mod upstream;

use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::{
    ConversationSink, PluginCtx, Provider, ProviderMeta, SendFuture, StoreAccount,
};
use reqwest::Method;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;
use upstream::{Failure, Wps};

const ID: &str = "wps-lingxi";
const LABEL: &str = "WPS 灵犀";
const LEDGER: &str = "wps-lingxi-daily";

#[derive(Default)]
struct Plugin {
    checkin_lock: Mutex<BTreeMap<String, std::sync::Weak<Mutex<()>>>>,
}

fn view(secret: &Value) -> Value {
    let nickname = secret["nickname"].as_str().unwrap_or("");
    let name = secret["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| Some(nickname).filter(|s| !s.is_empty()))
        .unwrap_or(LABEL);
    json!({"name": name, "uid": secret["uid"].as_str(), "nickname": nickname})
}

fn identity(secret: &Value) -> Result<String, String> {
    let uid = secret["uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("缺少已验证的 WPS 账号 ID")?;
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

async fn stored(ctx: &PluginCtx, id: &str) -> Result<StoreAccount, String> {
    let account = if id.is_empty() {
        ctx.store_list(ID)
            .await?
            .into_iter()
            .next()
            .ok_or("请先添加 WPS 灵犀账号")?
    } else {
        ctx.store_get(ID, id).await?
    };
    if account.provider != ID {
        return Err("账号提供商不匹配".into());
    }
    Ok(account)
}

fn connect(
    ctx: &PluginCtx,
    account: &StoreAccount,
    proxy: Option<&proto::WireProxy>,
) -> Result<Wps, String> {
    let sid = account.secret["wps_sid"]
        .as_str()
        .ok_or("缺少 WPS 登录凭证，请重新导入")?;
    Wps::new(ctx, sid, proxy, account.proxy.as_ref()).map_err(Failure::rpc)
}

async fn catalog(ctx: &PluginCtx, id: &str) -> Result<proto::ModelList, String> {
    let wps = connect(ctx, &stored(ctx, id).await?, None)?;
    let env = wps.get(upstream::PLANS).await.map_err(Failure::rpc)?;
    let rows = env["data"]["models"]
        .as_array()
        .ok_or("WPS 灵犀模型目录格式无效")?;
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
        return Err("WPS 灵犀未返回可用模型，请检查账号权限".into());
    }
    Ok(proto::ModelList {
        models: models.into_values().collect(),
        meta: proto::CatalogMeta {
            remote: true,
            refreshed_at: chrono::Utc::now().timestamp_millis(),
        },
    })
}

async fn daily_task(wps: &Wps) -> Result<Option<Value>, String> {
    let env = wps.get(upstream::TASKS).await.map_err(Failure::rpc)?;
    let tasks = env["data"]["tasks"]
        .as_array()
        .ok_or("WPS 灵犀任务列表格式无效")?;
    Ok(tasks
        .iter()
        .find(|task| task["task_key"] == "daily_check_in")
        .cloned())
}

fn report(outcome: proto::CheckinOutcome, message: &str, task: &Value) -> proto::CheckinReport {
    proto::CheckinReport {
        outcome,
        credit: None,
        message: Some(message.into()),
        streak: number(&task["extra"]["consecutive_days"]).and_then(|n| u32::try_from(n).ok()),
    }
}

impl Plugin {
    async fn daily(&self, ctx: &PluginCtx, id: &str) -> Result<proto::CheckinReport, String> {
        use proto::CheckinOutcome::*;
        let account = stored(ctx, id).await?;
        let lock = {
            let mut locks = self.checkin_lock.lock().await;
            locks.retain(|_, v| v.strong_count() > 0);
            let key = identity(&account.secret)?;
            let lock = locks
                .get(&key)
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_default();
            locks.insert(key, Arc::downgrade(&lock));
            lock
        };
        let _guard = lock.lock().await;
        let wps = connect(ctx, &account, None)?;
        let Some(task) = daily_task(&wps).await? else {
            return Ok(report(Inactive, "当前账号没有每日签到任务", &Value::Null));
        };
        if task["status"] == "claimed" {
            return Ok(report(Done, "今日已签到", &task));
        }
        if !matches!(task["status"].as_str(), Some("incomplete" | "claimable")) {
            return Ok(report(Inactive, "当前每日签到任务不可领取", &task));
        }
        let day = (chrono::Utc::now() + chrono::Duration::hours(8))
            .format("%Y-%m-%d")
            .to_string();
        let prior = ctx.ledger_get(&account.id, LEDGER).await?;
        let uncertain = "今日签到结果待确认，请在 WPS 灵犀检查；本插件不会重发领取";
        if prior["day"] == day {
            return Ok(if prior["state"] == "claimed" {
                report(Done, "今日已签到", &task)
            } else {
                report(Failed, uncertain, &task)
            });
        }
        ctx.ledger_put(&account.id, LEDGER, &json!({"day":day,"state":"pending"}))
            .await?;
        let attempt = wps
            .json(
                Method::POST,
                &format!("{}?date={day}", upstream::CLAIM),
                None,
            )
            .await;
        match attempt {
            Ok(env) if env["result"] == "ok" => {
                ctx.ledger_put(&account.id, LEDGER, &json!({"day":day,"state":"claimed"}))
                    .await?;
                let mut result = report(Claimed, "今日签到成功", &env["data"]);
                result.credit =
                    number(&env["data"]["extra"]["today_reward"]).map(|n| format!("{n} 智点"));
                Ok(result)
            }
            Err(e) if e.0 == 401 => Err(e.rpc()),
            _ => {
                // A timed-out write may still have succeeded. A read is safe; a second write is not.
                if let Ok(Some(after)) = daily_task(&wps).await {
                    if after["status"] == "claimed" {
                        return Ok(report(Done, "今日已签到", &after));
                    }
                }
                Ok(report(Failed, uncertain, &task))
            }
        }
    }
}

impl Provider for Plugin {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::new(ID, LABEL)
            .with_capabilities(proto::Capabilities {
                stateful: true,
                model_refresh: true,
                usage: true,
                checkin: true,
                sse_model_rewrite: true,
                ..Default::default()
            })
            .with_login(vec![proto::LoginMethod::Token {
                label: "WPS wps_sid".into(),
                placeholder: "WPS 灵犀 wps_sid".into(),
                extra_fields: vec![],
            }])
            .with_account_features(proto::AccountFeatures {
                usage: true,
                identifier: vec!["uid".into()],
                ..Default::default()
            })
            .with_checkin(proto::CheckinInfo {
                description: Some("领取 WPS 灵犀每日签到智点，资格与奖励以 WPS 返回为准".into()),
                ..Default::default()
            })
    }

    fn normalize_account<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        input: proto::NormalizeInput,
    ) -> SendFuture<'a, proto::NormalizedAccount> {
        Box::pin(async move {
            let manual = match input.source {
                proto::NormalizeSource::Manual { payload } => payload,
                _ => return Err("请在 WPS 灵犀账号表单中导入 wps_sid".into()),
            };
            let fields = match manual.get("methodIndex") {
                Some(method) if method.as_u64() == Some(0) => manual
                    .get("inputs")
                    .filter(|v| v.is_object())
                    .ok_or("WPS 灵犀账号表单格式无效")?,
                Some(_) => return Err("WPS 灵犀凭证导入方式无效".into()),
                None => &manual,
            };
            let raw = fields["wps_sid"]
                .as_str()
                .or_else(|| fields["token"].as_str())
                .ok_or("请填写 wps_sid")?;
            let sid = upstream::cookie_value(raw).map_err(Failure::rpc)?;
            let wps = Wps::new(ctx, sid, input.proxy.as_ref(), None).map_err(Failure::rpc)?;
            let env = wps.get(upstream::PROFILE).await.map_err(Failure::rpc)?;
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
                .ok_or("WPS 灵犀未返回有效的账号 ID，未保存凭证")?;
            let secret = json!({"wps_sid":sid,"uid":uid,"nickname":label(user["name"].as_str()),
                "name":label(input.name.as_deref().or_else(|| manual["name"].as_str()))});
            Ok(proto::NormalizedAccount {
                public: view(&secret),
                identity: identity(&secret)?,
                secret,
                desktop: false,
            })
        })
    }

    fn account_view(&self, secret: &Value) -> Result<Value, String> {
        Ok(view(secret))
    }
    fn account_identity(&self, secret: &Value) -> Result<String, String> {
        identity(secret)
    }

    fn admin_action<'a>(
        &'a self,
        _ctx: &'a PluginCtx,
        _id: &'a str,
        action: &'a str,
        _payload: &'a Value,
    ) -> SendFuture<'a, Value> {
        Box::pin(async move {
            if action == "auth.methods" {
                Ok(methods())
            } else {
                Err(agent2api_provider_sdk::unsupported(
                    "WPS 灵犀不支持此管理操作",
                ))
            }
        })
    }

    fn models<'a>(&'a self, ctx: &'a PluginCtx, id: &'a str) -> SendFuture<'a, proto::ModelList> {
        Box::pin(catalog(ctx, id))
    }

    fn refresh_models<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        id: &'a str,
        _force: bool,
    ) -> SendFuture<'a, proto::RefreshOutcome> {
        Box::pin(async move {
            Ok(proto::RefreshOutcome::refreshed(
                catalog(ctx, id).await?.models.len(),
            ))
        })
    }

    fn usage<'a>(&'a self, ctx: &'a PluginCtx, id: &'a str) -> SendFuture<'a, Value> {
        Box::pin(async move {
            let wps = connect(ctx, &stored(ctx, id).await?, None)?;
            let env = wps.get(upstream::BALANCE).await.map_err(Failure::rpc)?;
            let data = &env["data"];
            let n = number(
                data.get("total_balance")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&data["balance"]),
            )
            .ok_or_else(|| Failure(502, "WPS 灵犀未返回有效智点余额").rpc())?;
            // 智点是单一余额，不是周期额度。网关用 available 做余额展示、汇总与限制判断。
            Ok(json!({"provider":ID,"plan":LABEL,"available":n,"balance":n,"unit":"智点"}))
        })
    }

    fn checkin<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        id: &'a str,
    ) -> SendFuture<'a, proto::CheckinReport> {
        Box::pin(self.daily(ctx, id))
    }

    fn conversation<'a>(
        &'a self,
        ctx: &'a PluginCtx,
        req: proto::ConversationRequest,
        sink: ConversationSink<'a>,
    ) -> SendFuture<'a, ()> {
        Box::pin(async move {
            let account = stored(ctx, &req.account).await?;
            let wps = connect(ctx, &account, req.proxy.as_ref())?;
            chat::run(wps, req, sink).await
        })
    }

    fn classify(&self, status: u16, body: &Value) -> proto::ErrorClass {
        let Failure(code, message) = upstream::http_failure(status, body);
        match code {
            401 => proto::ErrorClass::TokenExpired {
                message: message.into(),
            },
            402 | 429 => proto::ErrorClass::QuotaLimited {
                message: message.into(),
                status: code,
                upstream_code: None,
                reset_at: None,
            },
            _ => proto::ErrorClass::Fatal {
                status: code,
                message: message.into(),
                upstream_code: None,
            },
        }
    }
}

fn main() {
    agent2api_provider_sdk::serve_blocking(vec![Box::new(Plugin::default())]);
}
