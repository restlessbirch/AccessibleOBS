//! Работа с Twitch: подключение, параметры канала, метки.
//!
//! Вынесено из main.rs: там уже соседствовали маршрутизация, авторизация,
//! OBS, DonationAlerts и Twitch, и любое изменение требовало держать в
//! голове половину приложения.

use super::*;

/// Единая обработка ответа Twitch.
///
/// Twitch на 4xx возвращает валидный JSON вида
/// `{"error":"Unauthorized","status":401,"message":"..."}`. Прежний код звал
/// `.json()` сразу, не глядя на статус, и отдавал это тело как успешный ответ —
/// панель радостно сообщала «Маркер создан», хотя не произошло ничего.
/// Для удалённого оператора ложный успех хуже честной ошибки: он узнает правду
/// от зрителей.
pub(crate) async fn twitch_json(
    response: reqwest::Response,
    what: &str,
) -> Result<Value, Response> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);

    if !status.is_success() {
        let detail = parsed
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(body.trim());
        let hint = match status.as_u16() {
            401 => " Токен Twitch истёк — подключите Twitch заново.",
            403 => " Не хватает прав. Проверьте scope приложения Twitch.",
            429 => " Twitch временно ограничил частоту запросов, повторите позже.",
            _ => "",
        };
        error!("Twitch {what}: {status} {detail}");
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": {
                "message": format!("{what}: Twitch ответил {}.{hint}", status.as_u16()),
                "detail": detail,
            }})),
        )
            .into_response());
    }

    // Часть ручек Twitch отвечает 204 без тела — это законный успех.
    Ok(if body.trim().is_empty() {
        json!({"ok": true})
    } else {
        parsed
    })
}

pub(crate) async fn twitch_status(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(twitch_status_value(&st).await))
}

pub(crate) async fn twitch_config_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let c = effective_twitch_config(&st);
    Ok(Json(json!({
        "client_id": c.client_id,
        "scopes": c.scopes,
    })))
}

pub(crate) async fn twitch_config_set(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let current = effective_twitch_config(&st);
    let client_id = required_trimmed(&body, "clientId", "Twitch client_id обязателен")
        .map_err(|e| bad(&e))?
        .to_string();
    let scopes = scopes_from_body(&body, "scopes", &current.scopes);
    save_secret("twitch_client_id", &client_id)
        .map_err(|e| err("Не удалось сохранить Twitch client_id", e))?;
    save_secret(
        "twitch_scopes",
        &serde_json::to_string(&scopes).unwrap_or_default(),
    )
    .map_err(|e| err("Не удалось сохранить Twitch scopes", e))?;
    if current.client_id != client_id {
        delete_secret("twitch_tokens").ok();
        delete_secret("twitch_device_code").ok();
        // Токен снят — подтверждение по нему больше не действует, иначе панель
        // ещё минуту показывала бы «подключено» для стёртого доступа.
        forget_twitch_identity(&st).await;
    }
    save_host_config_update(|cfg| {
        cfg.twitch.client_id = client_id.clone();
        cfg.twitch.scopes = scopes.clone();
    })
    .map_err(|e| err("Не удалось обновить config/host.json", e))?;
    Ok(Json(json!({
        "ok": true,
        "client_id": client_id,
        "scopes": scopes,
    })))
}

pub(crate) async fn twitch_status_value(st: &AppState) -> Value {
    let cfg = effective_twitch_config(st);
    if cfg.client_id.is_empty() {
        return json!({
            "enabled": cfg.enabled,
            "configured": false,
            "connected": false,
            "message": "Сохраните Twitch client_id в веб-панели",
        });
    }
    if load_secret("twitch_tokens").ok().flatten().is_none() {
        return json!({"enabled": true, "configured": true, "connected": false});
    }
    // Ниже раньше лежала вторая проверка токена — свой запрос к validate на
    // случай, если первая ветка не сработала. Сработать она не могла: обе
    // ветки спрашивали одно и то же хранилище, и до второй дело доходило
    // только при отсутствии токена, где её ветка Some не выполнялась никогда.
    match twitch_user_refreshed(st).await {
        Ok(_) => json!({"enabled": true, "configured": true, "connected": true}),
        Err(e) => json!({
            "enabled": true,
            "configured": true,
            "connected": false,
            "message": format!("{e:#}"),
        }),
    }
}

pub(crate) async fn twitch_device_start(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let cfg = effective_twitch_config(&st);
    if cfg.client_id.is_empty() {
        return Err(bad("Сначала сохраните Twitch client_id в веб-панели"));
    }
    let scopes = cfg.scopes.join(" ");
    let form = [
        ("client_id", cfg.client_id.as_str()),
        ("scopes", scopes.as_str()),
    ];
    let response = st
        .http
        .post("https://id.twitch.tv/oauth2/device")
        .form(&form)
        .send()
        .await
        .map_err(|e| err("Twitch недоступен", e.into()))?;
    let v = twitch_json(response, "Запрос кода подключения").await?;
    if let Some(code) = v.get("device_code").and_then(Value::as_str) {
        save_secret("twitch_device_code", code).ok();
    }
    Ok(Json(v))
}

pub(crate) async fn twitch_device_check(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let cfg = effective_twitch_config(&st);
    let dc = load_secret("twitch_device_code")
        .map_err(|e| err("Не удалось прочитать device code", e))?
        .ok_or_else(|| bad("Сначала нажмите «Подключить Twitch»"))?;
    let form = [
        ("client_id", cfg.client_id.as_str()),
        ("device_code", dc.as_str()),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];
    let response = st
        .http
        .post("https://id.twitch.tv/oauth2/token")
        .form(&form)
        .send()
        .await
        .map_err(|e| err("Не удалось проверить авторизацию Twitch", e.into()))?;

    // Здесь twitch_json намеренно НЕ используется. Пока владелец не ввёл код на
    // сайте Twitch, endpoint отвечает 400 с authorization_pending — это штатное
    // ожидание, а не сбой, и превращать его в ошибку нельзя: кнопка «Проверить
    // авторизацию» ругалась бы при каждом нажатии до подтверждения.
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let v = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    let message = v.get("message").and_then(Value::as_str).unwrap_or("");

    if v.get("access_token").is_some() {
        save_secret("twitch_tokens", &v.to_string())
            .map_err(|e| err("Не удалось сохранить Twitch token", e))?;
        delete_secret("twitch_device_code").ok();
        // Пришёл новый токен: прежнее подтверждение относилось к другому.
        forget_twitch_identity(&st).await;
        return Ok(Json(json!({"status": "connected"})));
    }
    if message.contains("authorization_pending") {
        return Ok(Json(json!({
            "status": "pending",
            "message": "Код ещё не подтверждён на сайте Twitch.",
        })));
    }
    if message.contains("expired") {
        return Ok(Json(json!({
            "status": "expired",
            "message": "Код устарел. Нажмите «Подключить Twitch» заново.",
        })));
    }
    error!("Twitch device check: {status} {body}");
    Err((
        StatusCode::BAD_GATEWAY,
        Json(json!({"error": {
            "message": "Twitch отклонил проверку авторизации.",
            "detail": if message.is_empty() { body.trim() } else { message },
        }})),
    )
        .into_response())
}

/// Подтверждённая личность подключённого канала.
///
/// Логин и user_id приходят одним ответом validate, поэтому и хранятся вместе.
/// Раньше [`twitch_channel_login`] брал user_id через `twitch_user_refreshed`,
/// а потом спрашивал validate второй раз ради логина — два одинаковых запроса
/// к Twitch там, где хватает одного.
#[derive(Clone)]
pub(crate) struct TwitchIdentity {
    pub access: String,
    pub user_id: String,
    pub login: Option<String>,
    checked_at: Instant,
}

/// Сколько доверять уже подтверждённому токену.
///
/// Панель зовёт /api/health и /api/twitch/status после каждой команды
/// оператора, а каждый такой вызов упирался в сетевой запрос к Twitch: два
/// обращения на одно нажатие кнопки плюс задержка ответа, которую незрячий
/// оператор ждёт, не понимая, прошла команда или нет. Сам Twitch просит
/// проверять токен раз в час, так что минута кэша ничего не портит, а отзыв
/// доступа на стороне Twitch мы заметим на следующей минуте.
const TWITCH_IDENTITY_TTL: Duration = Duration::from_secs(60);

/// Забыть подтверждённую личность: вызывается там, где токен заменён или снят.
pub(crate) async fn forget_twitch_identity(st: &AppState) {
    *st.twitch_identity.write().await = None;
}

pub(crate) async fn twitch_user_refreshed(st: &AppState) -> Result<TwitchIdentity> {
    if let Some(cached) = st.twitch_identity.read().await.as_ref()
        && cached.checked_at.elapsed() < TWITCH_IDENTITY_TTL
    {
        return Ok(cached.clone());
    }
    let identity = twitch_identity_from_network(st).await?;
    *st.twitch_identity.write().await = Some(identity.clone());
    Ok(identity)
}

async fn twitch_identity_from_network(st: &AppState) -> Result<TwitchIdentity> {
    let tok = load_secret("twitch_tokens")?.ok_or_else(|| anyhow!("Twitch не подключён"))?;
    let v: Value = serde_json::from_str(&tok)?;
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("access_token отсутствует"))?;
    match validate_twitch_access(st, access).await {
        Ok((user_id, login)) => Ok(TwitchIdentity {
            access: access.to_string(),
            user_id,
            login,
            checked_at: Instant::now(),
        }),
        Err(first_error) => {
            let Some(refresh) = v.get("refresh_token").and_then(Value::as_str) else {
                return Err(first_error);
            };
            let refreshed = refresh_twitch_tokens(st, refresh).await?;
            let access = refreshed
                .get("access_token")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Twitch не вернул новый access_token"))?;
            save_secret("twitch_tokens", &refreshed.to_string())?;
            let (user_id, login) = validate_twitch_access(st, access).await?;
            Ok(TwitchIdentity {
                access: access.to_string(),
                user_id,
                login,
                checked_at: Instant::now(),
            })
        }
    }
}

/// Возвращает user_id и логин: validate отдаёт их одним ответом.
pub(crate) async fn validate_twitch_access(
    st: &AppState,
    access: &str,
) -> Result<(String, Option<String>)> {
    let response = st
        .http
        .get("https://id.twitch.tv/oauth2/validate")
        .bearer_auth(access)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let val: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = val
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(body.trim());
        return Err(anyhow!(
            "Twitch token не прошёл validate: {status} {detail}"
        ));
    }
    let user_id = val
        .get("user_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Twitch не вернул user_id"))?;
    let login = val
        .get("login")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|login| !login.is_empty())
        .map(str::to_string);
    Ok((user_id, login))
}

pub(crate) async fn twitch_channel_login(st: &AppState) -> Result<Option<String>> {
    let cfg = effective_twitch_config(st);
    if !cfg.enabled || cfg.client_id.is_empty() || load_secret("twitch_tokens")?.is_none() {
        return Ok(None);
    }
    Ok(twitch_user_refreshed(st).await?.login)
}

pub(crate) async fn refresh_twitch_tokens(st: &AppState, refresh: &str) -> Result<Value> {
    let cfg = effective_twitch_config(st);
    let form = [
        ("client_id", cfg.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
    ];
    let response = st
        .http
        .post("https://id.twitch.tv/oauth2/token")
        .form(&form)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let val: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = val
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(body.trim());
        return Err(anyhow!("Twitch refresh_token отклонён: {status} {detail}"));
    }
    if val.get("access_token").is_none() {
        return Err(anyhow!("Twitch не вернул access_token при обновлении"));
    }
    Ok(val)
}

pub(crate) async fn twitch_channel_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let cfg = effective_twitch_config(&st);
    let me = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let response = st
        .http
        .get("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", me.user_id.as_str())])
        .header("Client-Id", &cfg.client_id)
        .bearer_auth(&me.access)
        .send()
        .await
        .map_err(|e| err("Twitch недоступен", e.into()))?;
    Ok(Json(
        twitch_json(response, "Чтение параметров канала").await?,
    ))
}

pub(crate) async fn twitch_channel_modify(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let cfg = effective_twitch_config(&st);
    let me = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let response = st
        .http
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", me.user_id.as_str())])
        .header("Client-Id", &cfg.client_id)
        .bearer_auth(&me.access)
        .json(&body)
        .send()
        .await
        .map_err(|e| err("Не удалось изменить канал Twitch", e.into()))?;
    // Успех здесь — 204 без тела; twitch_json превратит его в {"ok": true},
    // а любую ошибку — в настоящий код ошибки вместо «ok: false» внутри 200.
    Ok(Json(twitch_json(response, "Изменение канала").await?))
}

pub(crate) async fn twitch_marker(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let cfg = effective_twitch_config(&st);
    let me = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let description = body
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let response = st
        .http
        .post("https://api.twitch.tv/helix/streams/markers")
        .header("Client-Id", &cfg.client_id)
        .bearer_auth(&me.access)
        .json(&json!({"user_id": me.user_id, "description": description}))
        .send()
        .await
        .map_err(|e| err("Не удалось создать метку Twitch", e.into()))?;
    Ok(Json(twitch_json(response, "Создание метки").await?))
}
