use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ObsClient {
    pub host: String,
    pub port: u16,
    pub password: String,
}
impl ObsClient {
    pub fn new(host: impl Into<String>, port: u16, password: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            password: password.into(),
        }
    }
    pub async fn request(&self, request_type: &str, request_data: Value) -> Result<Value> {
        let url = format!("ws://{}:{}", self.host, self.port);
        let (mut ws, _) = connect_async(url)
            .await
            .context("OBS WebSocket недоступен")?;
        let hello = ws
            .next()
            .await
            .ok_or_else(|| anyhow!("OBS не прислал Hello"))??;
        let hello_json: Value = serde_json::from_str(hello.to_text()?)?;
        if hello_json.get("op").and_then(|v| v.as_i64()) != Some(0) {
            bail!("Неожиданный ответ OBS при подключении");
        }
        let mut identify = json!({"rpcVersion": 1, "eventSubscriptions": 0});
        if let Some(auth) = hello_json.pointer("/d/authentication") {
            if self.password.is_empty() {
                bail!(
                    "OBS WebSocket требует пароль. Запустите START_FRIEND.bat, он сгенерирует и сохранит пароль автоматически."
                );
            }
            let challenge = auth
                .get("challenge")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let salt = auth
                .get("salt")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            identify["authentication"] = Value::String(obs_auth(&self.password, salt, challenge));
        }
        ws.send(Message::Text(
            json!({"op":1,"d":identify}).to_string().into(),
        ))
        .await?;
        loop {
            let msg = ws
                .next()
                .await
                .ok_or_else(|| anyhow!("OBS закрыл соединение до Identified"))??;
            let v: Value = serde_json::from_str(msg.to_text()?)?;
            match v.get("op").and_then(|v| v.as_i64()) {
                Some(2) => break,
                Some(9) => bail!("OBS отклонил авторизацию"),
                _ => {}
            }
        }
        let request_id = Uuid::new_v4().to_string();
        ws.send(Message::Text(json!({"op":6,"d":{"requestType":request_type,"requestId":request_id,"requestData":request_data}}).to_string().into())).await?;
        while let Some(msg) = ws.next().await {
            let msg = msg?;
            if !msg.is_text() {
                continue;
            }
            let v: Value = serde_json::from_str(msg.to_text()?)?;
            if v.get("op").and_then(|v| v.as_i64()) == Some(7)
                && v.pointer("/d/requestId").and_then(|v| v.as_str()) == Some(&request_id)
            {
                let status = v.pointer("/d/requestStatus").cloned().unwrap_or(json!({}));
                if status.get("result").and_then(|v| v.as_bool()) == Some(true) {
                    return Ok(v.pointer("/d/responseData").cloned().unwrap_or(json!({})));
                }
                let comment = status
                    .get("comment")
                    .and_then(|v| v.as_str())
                    .unwrap_or("OBS command failed");
                bail!(comment.to_string());
            }
        }
        bail!("OBS не вернул ответ на команду")
    }
    pub async fn status(&self) -> Value {
        match self.request("GetVersion", json!({})).await {
            Ok(v) => json!({"connected":true,"version":v}),
            Err(e) => json!({"connected":false,"error":friendly_obs_error(&e.to_string())}),
        }
    }
}

fn obs_auth(password: &str, salt: &str, challenge: &str) -> String {
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    h.update(salt.as_bytes());
    let secret = general_purpose::STANDARD.encode(h.finalize());
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(challenge.as_bytes());
    general_purpose::STANDARD.encode(h.finalize())
}
fn friendly_obs_error(e: &str) -> String {
    if e.contains("ECONNREFUSED") || e.contains("connection refused") {
        "Не удалось подключиться к OBS. Проверьте, что OBS запущен.".into()
    } else if e.contains("authentication") || e.contains("авториза") {
        "Не удалось авторизоваться в OBS WebSocket.".into()
    } else {
        e.to_string()
    }
}

pub fn db_to_mul(db: f64) -> f64 {
    if db <= -100.0 {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}
pub fn mul_to_db(mul: f64) -> f64 {
    if mul <= 0.0 {
        -100.0
    } else {
        20.0 * mul.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversion_round_trips_common_values() {
        for db in [-60.0, -24.0, -8.0, -1.0, 0.0, 6.0] {
            let round_trip = mul_to_db(db_to_mul(db));
            assert!((round_trip - db).abs() < 0.000001);
        }
    }

    #[test]
    fn db_floor_maps_to_zero_multiplier() {
        assert_eq!(db_to_mul(-100.0), 0.0);
        assert_eq!(mul_to_db(0.0), -100.0);
    }

    #[test]
    fn obs_auth_matches_protocol_shape() {
        let value = obs_auth("password", "salt", "challenge");
        assert!(!value.is_empty());
        assert!(
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .is_ok()
        );
    }
}
