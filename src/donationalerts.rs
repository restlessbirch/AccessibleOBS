//! Realtime-лента донатов DonationAlerts через Centrifugo.
//!
//! Важно понимать разделение ответственности:
//!
//! * **Озвучка доната у актёра** — работа официального Alerts Widget. Он живёт
//!   в OBS как browser_source с `reroute_audio`, а TTS включается в личном
//!   кабинете DonationAlerts. Этот модуль на озвучку не влияет вообще.
//! * **Список донатов у модератора** — работа этого модуля. Он подписывается на
//!   канал `$alerts:donation_<id>` и отдаёт события в панель.
//!
//! Поэтому отсутствие OAuth-токенов ломает только ленту в панели, но не эфир.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::sync::{RwLock, broadcast};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use crate::{DonationAlertsConfig, load_secret, save_secret};

const CENTRIFUGO_URL: &str = "wss://centrifugo.donationalerts.com/connection/websocket";
const USER_URL: &str = "https://www.donationalerts.com/api/v1/user/oauth";
const SUBSCRIBE_URL: &str = "https://www.donationalerts.com/api/v1/centrifuge/subscribe";
const TOKEN_URL: &str = "https://www.donationalerts.com/oauth/token";

const MAX_RECENT: usize = 50;
const PING_INTERVAL: Duration = Duration::from_secs(25);
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Пока токенов нет, опрашиваем хранилище редко — это не ошибка, а норма
/// до того, как владелец пройдёт OAuth.
const IDLE_POLL: Duration = Duration::from_secs(30);

// Коды методов протокола Centrifugo v2, который использует DonationAlerts.
const METHOD_SUBSCRIBE: i64 = 1;
const METHOD_PING: i64 = 7;

#[derive(Debug, Clone, Default, Serialize)]
pub struct FeedStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Общая лента донатов: последние события плюс broadcast для SSE.
#[derive(Clone)]
pub struct DonationFeed {
    recent: Arc<RwLock<VecDeque<Value>>>,
    events: broadcast::Sender<Value>,
    status: Arc<RwLock<FeedStatus>>,
}

impl DonationFeed {
    pub fn new(events: broadcast::Sender<Value>) -> Self {
        Self {
            recent: Arc::new(RwLock::new(VecDeque::new())),
            events,
            status: Arc::new(RwLock::new(FeedStatus::default())),
        }
    }

    pub async fn recent(&self) -> Vec<Value> {
        self.recent.read().await.iter().cloned().collect()
    }

    pub async fn status(&self) -> FeedStatus {
        self.status.read().await.clone()
    }

    async fn push(&self, donation: Value) {
        {
            let mut recent = self.recent.write().await;
            recent.push_front(donation.clone());
            while recent.len() > MAX_RECENT {
                recent.pop_back();
            }
        }
        let _ = self.events.send(json!({
            "type": "donation",
            "donation": donation,
        }));
    }

    async fn set_status(&self, status: FeedStatus) {
        let changed = {
            let mut guard = self.status.write().await;
            let changed = guard.connected != status.connected;
            *guard = status.clone();
            changed
        };
        if changed {
            let _ = self.events.send(json!({
                "type": "donationalerts_status",
                "status": status,
            }));
        }
    }
}

/// Способ получить актуальные настройки перед каждой попыткой подключения.
///
/// Именно функция, а не готовая структура: владелец может сменить client_id и
/// client_secret прямо из панели. Со снимком, взятым при запуске процесса,
/// воркер продолжал бы жить со старыми данными — и подвох вылез бы не сразу,
/// а в момент обновления токена, когда текущий access_token истечёт.
pub type ConfigSource = Arc<dyn Fn() -> DonationAlertsConfig + Send + Sync>;

/// Держит подписку живой, переподключаясь при обрывах и ожидая появления токенов.
pub fn spawn(config: ConfigSource, http: reqwest::Client, feed: DonationFeed) {
    tokio::spawn(async move {
        let mut backoff = RETRY_MIN;
        loop {
            let cfg = config();
            if load_secret("donationalerts_tokens")
                .ok()
                .flatten()
                .is_none()
            {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }
            match session(&cfg, &http, &feed).await {
                Ok(()) => {
                    info!("DonationAlerts: подписка закрыта, переподключаюсь");
                    backoff = RETRY_MIN;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    warn!("DonationAlerts realtime: {}", msg);
                    feed.set_status(FeedStatus {
                        connected: false,
                        user_name: None,
                        error: Some(msg),
                    })
                    .await;
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RETRY_MAX);
        }
    });
}

async fn session(
    cfg: &DonationAlertsConfig,
    http: &reqwest::Client,
    feed: &DonationFeed,
) -> Result<()> {
    let access = access_token(cfg, http).await?;
    let user = fetch_user(http, &access).await?;
    let user_id = user
        .pointer("/data/id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("DonationAlerts не вернул id пользователя"))?;
    let socket_token = user
        .pointer("/data/socket_connection_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("DonationAlerts не вернул socket_connection_token"))?
        .to_string();
    let user_name = user
        .pointer("/data/name")
        .and_then(Value::as_str)
        .map(str::to_string);

    let (ws, _) = connect_async(CENTRIFUGO_URL)
        .await
        .context("Centrifugo недоступен")?;
    let (mut sink, mut stream) = ws.split();

    // Centrifugo v2: метод по умолчанию (0) — connect.
    sink.send(Message::Text(
        json!({"params": {"token": socket_token}, "id": 1})
            .to_string()
            .into(),
    ))
    .await?;
    let connect_reply = read_reply(&mut stream, 1).await?;
    let client_id = connect_reply
        .pointer("/result/client")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Centrifugo не вернул client id"))?
        .to_string();

    // Приватный канал требует подписи со стороны DonationAlerts.
    let channel = format!("$alerts:donation_{user_id}");
    let channel_token = sign_channel(http, &access, &channel, &client_id).await?;
    sink.send(Message::Text(
        json!({
            "params": {"channel": channel, "token": channel_token},
            "method": METHOD_SUBSCRIBE,
            "id": 2
        })
        .to_string()
        .into(),
    ))
    .await?;
    read_reply(&mut stream, 2).await?;

    feed.set_status(FeedStatus {
        connected: true,
        user_name,
        error: None,
    })
    .await;
    info!("DonationAlerts: подписка на {} активна", channel);

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    let mut ping_id = 100i64;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(msg) = incoming else { break };
                match msg? {
                    Message::Text(text) => {
                        for donation in extract_donations(&text) {
                            feed.push(donation).await;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                ping_id += 1;
                sink.send(Message::Text(
                    json!({"method": METHOD_PING, "id": ping_id}).to_string().into(),
                ))
                .await?;
            }
        }
    }

    feed.set_status(FeedStatus {
        connected: false,
        user_name: None,
        error: None,
    })
    .await;
    Ok(())
}

/// Centrifugo шлёт push-и без `id`, а ответы на команды — с `id`.
/// Несколько сообщений могут прийти в одном кадре, разделённые переводом строки.
async fn read_reply<S>(stream: &mut S, id: i64) -> Result<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let msg = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| anyhow!("Centrifugo не ответил на команду {id}"))?;
        let Some(msg) = msg else {
            bail!("Centrifugo закрыл соединение");
        };
        let Message::Text(text) = msg? else { continue };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = v.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("неизвестная ошибка");
                bail!("Centrifugo отклонил команду {id}: {message}");
            }
            return Ok(v);
        }
    }
}

/// Достаёт полезную нагрузку донатов из кадра Centrifugo.
/// Формат push-а: `{"result":{"channel":"...","data":{"data":{...}}}}`.
fn extract_donations(frame: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in frame.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("id").is_some() {
            continue; // ответ на команду, не push
        }
        if let Some(data) = v.pointer("/result/data/data") {
            out.push(data.clone());
        }
    }
    out
}

async fn fetch_user(http: &reqwest::Client, access: &str) -> Result<Value> {
    let r = http
        .get(USER_URL)
        .bearer_auth(access)
        .send()
        .await
        .context("DonationAlerts API недоступен")?;
    if !r.status().is_success() {
        bail!("DonationAlerts /user/oauth вернул {}", r.status());
    }
    r.json().await.context("непонятный ответ /user/oauth")
}

async fn sign_channel(
    http: &reqwest::Client,
    access: &str,
    channel: &str,
    client_id: &str,
) -> Result<String> {
    let r = http
        .post(SUBSCRIBE_URL)
        .bearer_auth(access)
        .json(&json!({"channels": [channel], "client": client_id}))
        .send()
        .await
        .context("не удалось подписать канал DonationAlerts")?;
    if !r.status().is_success() {
        bail!("DonationAlerts /centrifuge/subscribe вернул {}", r.status());
    }
    let v: Value = r.json().await.context("непонятный ответ subscribe")?;
    v.pointer("/channels/0/token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("DonationAlerts не вернул токен канала"))
}

/// Возвращает рабочий access_token, при необходимости обновляя его по refresh_token.
async fn access_token(cfg: &DonationAlertsConfig, http: &reqwest::Client) -> Result<String> {
    let raw = load_secret("donationalerts_tokens")?
        .ok_or_else(|| anyhow!("DonationAlerts OAuth ещё не пройден"))?;
    let tokens: Value = serde_json::from_str(&raw).context("повреждённые токены DonationAlerts")?;
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("в сохранённых данных нет access_token"))?;

    if token_is_valid(http, access).await {
        return Ok(access.to_string());
    }

    let refresh = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("access_token истёк, а refresh_token не сохранён"))?;
    let refreshed = refresh_tokens(cfg, http, refresh).await?;
    let access = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("DonationAlerts не вернул новый access_token"))?
        .to_string();
    save_secret("donationalerts_tokens", &refreshed.to_string())?;
    info!("DonationAlerts: токен обновлён по refresh_token");
    Ok(access)
}

async fn token_is_valid(http: &reqwest::Client, access: &str) -> bool {
    http.get(USER_URL)
        .bearer_auth(access)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn refresh_tokens(
    cfg: &DonationAlertsConfig,
    http: &reqwest::Client,
    refresh: &str,
) -> Result<Value> {
    if cfg.client_id.is_empty() || cfg.client_secret.is_empty() {
        bail!(
            "для обновления токена нужны donationalerts.client_id и client_secret в config/host.json"
        );
    }
    let scope = cfg.oauth_scopes.join(" ");
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", cfg.client_id.as_str()),
        ("client_secret", cfg.client_secret.as_str()),
        ("scope", scope.as_str()),
    ];
    let r = http
        .post(TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("token endpoint DonationAlerts недоступен")?;
    if !r.status().is_success() {
        bail!("обновление токена DonationAlerts вернуло {}", r.status());
    }
    r.json().await.context("непонятный ответ token endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_donation_from_centrifugo_push() {
        let frame = r#"{"result":{"channel":"$alerts:donation_1","data":{"data":{"id":42,"username":"Гость","amount":100,"currency":"RUB","message":"Привет"}}}}"#;
        let out = extract_donations(frame);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 42);
        assert_eq!(out[0]["username"], "Гость");
    }

    #[test]
    fn ignores_command_replies_and_blank_lines() {
        let frame = "{\"id\":2,\"result\":{}}\n\n{\"id\":1,\"result\":{\"client\":\"abc\"}}";
        assert!(extract_donations(frame).is_empty());
    }

    #[test]
    fn handles_multiple_pushes_in_one_frame() {
        let frame = concat!(
            r#"{"result":{"channel":"c","data":{"data":{"id":1}}}}"#,
            "\n",
            r#"{"result":{"channel":"c","data":{"data":{"id":2}}}}"#
        );
        let out = extract_donations(frame);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["id"], 2);
    }

    #[tokio::test]
    async fn feed_keeps_newest_first_and_caps_history() {
        let (tx, _rx) = broadcast::channel(8);
        let feed = DonationFeed::new(tx);
        for i in 0..(MAX_RECENT + 10) {
            feed.push(json!({"id": i})).await;
        }
        let recent = feed.recent().await;
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0]["id"], MAX_RECENT + 9);
    }

    #[tokio::test]
    async fn status_change_is_broadcast_once() {
        let (tx, mut rx) = broadcast::channel(8);
        let feed = DonationFeed::new(tx);
        feed.set_status(FeedStatus {
            connected: true,
            user_name: Some("tester".into()),
            error: None,
        })
        .await;
        let msg = rx.try_recv().expect("статус разослан");
        assert_eq!(msg["type"], "donationalerts_status");
        assert_eq!(msg["status"]["connected"], true);
    }
}
