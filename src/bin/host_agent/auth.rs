//! Авторизация панели: pairing-код, сессии, происхождение запроса.
//!
//! Вынесено из main.rs. Здесь же проверка Origin и Host — единственное, что
//! отделяет панель от постороннего в локальном режиме, где pairing-кода нет.

use super::*;

pub(crate) fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

/// Сравнение за постоянное время: обычный `==` выходит на первом различии и
/// теоретически позволяет подбирать токен по времени ответа.
pub(crate) fn secret_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Пришёл ли запрос со страницы самой панели, а не с чужого сайта.
///
/// В локальном режиме pairing-кода нет, поэтому единственное, что отделяет
/// панель от постороннего, — происхождение запроса. Без этой проверки любая
/// открытая в браузере страница могла бы слать POST на 127.0.0.1:8787 и
/// управлять чужим эфиром: браузер сам приложит куки, а подтверждения
/// незрячий оператор не увидит.
///
/// Заодно отсекается перепривязка DNS: браузер обратится по чужому имени,
/// и Host не совпадёт с петлевым адресом.
pub(crate) fn origin_is_trusted(headers: &HeaderMap) -> bool {
    // Локальный режим слушает только петлевой интерфейс, поэтому и доверяем
    // только ему. Имя localhost допускаем: браузеры часто подставляют его.
    let host_allowed = |value: &str| {
        let host = value.rsplit_once(':').map_or(value, |(h, _)| h);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    };

    // Origin присылают браузеры при запросах, меняющих состояние.
    if let Some(origin) = headers.get("origin") {
        // Заголовок есть, но прочитать его не выходит — доверять нечему.
        // Раньше здесь был провал в следующую ветку, и запрос с нечитаемым
        // Origin проходил как доверенный.
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        return match origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
        {
            Some(rest) => host_allowed(rest),
            // "null" и прочее нестандартное происхождение доверия не заслуживают.
            None => false,
        };
    }

    // Origin нет — проверяем адрес, по которому к нам обратились.
    // Отсутствие Host тоже считаем недоверенным: в HTTP/1.1 он обязателен,
    // и запрос без него приходит не от панели.
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .is_some_and(host_allowed)
}

pub(crate) async fn is_auth(st: &AppState, headers: &HeaderMap) -> bool {
    if st.cfg.runtime_mode == RuntimeMode::Local {
        // Пароля нет, поэтому доверие держится на происхождении запроса.
        return origin_is_trusted(headers);
    }
    let Some(presented) = cookie(headers, "aobs_session") else {
        return false;
    };
    let now = now_unix();
    let mut guard = st.session.write().await;
    let Some(session) = guard.as_mut() else {
        return false;
    };
    if session.expired(now) {
        *guard = None;
        delete_secret("session_token").ok();
        return false;
    }
    if secret_eq(&presented, &session.token) {
        session.touch(now);
        true
    } else {
        false
    }
}

pub(crate) async fn require_auth(st: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if is_auth(st, headers).await {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Требуется pairing-код Accessible OBS."}})),
        )
            .into_response())
    }
}

pub(crate) async fn auth_status(State(st): State<AppState>, headers: HeaderMap) -> Json<Value> {
    if st.cfg.runtime_mode == RuntimeMode::Local {
        return Json(json!({
            "paired": true,
            "authenticated": true,
            "mode": "local",
        }));
    }
    Json(json!({
        "paired": load_secret("pairing_secret").ok().flatten().is_some(),
        "authenticated": is_auth(&st, &headers).await,
    }))
}

pub(crate) async fn auth_login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
    if st.cfg.runtime_mode == RuntimeMode::Local {
        return Json(json!({"ok": true, "mode": "local"})).into_response();
    }
    let peer_ip = peer.ip();
    {
        let mut guards = st.login_guards.lock().await;
        let guard = guards.entry(peer_ip).or_default();
        if let Some(left) = guard.blocked_for() {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": {"message": format!(
                    "Слишком много неудачных попыток. Подождите {} секунд.",
                    left.as_secs() + 1
                )}})),
            )
                .into_response();
        }
    }

    let Some(secret) = body.get("secret").and_then(Value::as_str) else {
        return bad("Введите pairing-код.");
    };
    let Ok(Some(real)) = load_secret("pairing_secret") else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"message": "Pairing-код ещё не создан. Запустите START_FRIEND.bat на компьютере актёра."}})),
        )
            .into_response();
    };

    if !secret_eq(secret.trim(), &real) {
        st.login_guards
            .lock()
            .await
            .entry(peer_ip)
            .or_default()
            .record_failure();
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Неверный pairing-код."}})),
        )
            .into_response();
    }

    st.login_guards
        .lock()
        .await
        .entry(peer_ip)
        .or_default()
        .record_success();
    let session = StoredSession::new(random_secret_b64(32), now_unix());
    if let Err(e) = save_session_secret(&session) {
        return err("Не удалось сохранить сессию", e);
    }
    *st.session.write().await = Some(session.clone());

    let mut resp = Json(json!({"ok": true})).into_response();
    // Secure не ставим намеренно: связь идёт по http внутри tailnet, где
    // шифрование обеспечивает сам WireGuard. С флагом Secure куку бы отбросили.
    resp.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&format!(
            "aobs_session={}; Path=/; HttpOnly; SameSite=Lax",
            session.token
        ))
        .expect("токен из base64 всегда валиден для заголовка"),
    );
    resp
}

pub(crate) async fn auth_logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if st.cfg.runtime_mode == RuntimeMode::Local {
        return Json(json!({"ok": true, "mode": "local"})).into_response();
    }
    if let Err(resp) = require_auth(&st, &headers).await {
        return resp;
    }
    *st.session.write().await = None;
    let _ = delete_secret("session_token");
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static("aobs_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("cookie", HeaderValue::from_str(value).unwrap());
        h
    }

    fn with_header(name: &'static str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn cookie_parses_target_among_many() {
        let h = headers_with_cookie("a=1; aobs_session=token-value; b=2");
        assert_eq!(cookie(&h, "aobs_session").as_deref(), Some("token-value"));
    }

    #[test]
    fn cookie_absent_returns_none() {
        let h = headers_with_cookie("other=1");
        assert!(cookie(&h, "aobs_session").is_none());
    }

    #[test]
    fn secret_eq_rejects_different_lengths_and_values() {
        assert!(secret_eq("abcdef", "abcdef"));
        assert!(!secret_eq("abcdef", "abcdeg"));
        assert!(!secret_eq("abcdef", "abcde"));
        assert!(!secret_eq("", "x"));
    }

    #[test]
    fn login_guard_blocks_after_repeated_failures() {
        let mut g = LoginGuard::default();
        for _ in 0..(MAX_LOGIN_FAILURES - 1) {
            g.record_failure();
            assert!(g.blocked_for().is_none());
        }
        g.record_failure();
        let left = g.blocked_for().expect("блокировка включилась");
        assert!(left <= LOGIN_BLOCK);
    }

    #[test]
    fn login_guard_resets_after_success() {
        let mut g = LoginGuard::default();
        g.record_failure();
        g.record_failure();
        g.record_success();
        assert!(g.blocked_for().is_none());
        assert_eq!(g.failures, 0);
    }

    #[test]
    fn panel_origin_is_trusted() {
        assert!(origin_is_trusted(&with_header(
            "origin",
            "http://127.0.0.1:8787"
        )));
        assert!(origin_is_trusted(&with_header(
            "origin",
            "http://localhost:8787"
        )));
        assert!(origin_is_trusted(&with_header(
            "origin",
            "http://[::1]:8787"
        )));
    }

    #[test]
    fn foreign_site_cannot_drive_local_mode() {
        // В локальном режиме pairing-кода нет, поэтому чужая страница,
        // открытая в том же браузере, иначе управляла бы эфиром: браузер сам
        // приложил бы куки, а незрячий оператор ничего бы не заметил.
        assert!(!origin_is_trusted(&with_header(
            "origin",
            "https://evil.example"
        )));
        assert!(!origin_is_trusted(&with_header(
            "origin",
            "http://192.168.1.50"
        )));
        assert!(!origin_is_trusted(&with_header("origin", "null")));
        // Похожее имя не должно проходить как localhost.
        assert!(!origin_is_trusted(&with_header(
            "origin",
            "http://localhost.evil.example"
        )));
    }

    #[test]
    fn dns_rebinding_is_rejected_by_host() {
        // Браузер обращается по чужому имени, которое указывает на 127.0.0.1.
        // Origin при простых запросах может отсутствовать, но Host выдаёт подмену.
        assert!(!origin_is_trusted(&with_header(
            "host",
            "evil.example:8787"
        )));
        assert!(origin_is_trusted(&with_header("host", "127.0.0.1:8787")));
    }

    #[test]
    fn request_without_recognisable_origin_is_refused() {
        // Fail-closed: пустые заголовки не должны означать «свой».
        assert!(!origin_is_trusted(&HeaderMap::new()));
    }
}
