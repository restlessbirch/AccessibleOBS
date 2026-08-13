// В релизе агент работает фоном без консольного окна — иначе при каждом входе
// в Windows у актёра мигало бы чёрное окно. В отладке консоль оставляем.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use remote_stream_control::{
    donationalerts::{self, DonationFeed},
    health::{self, HealthWatch},
    obs::{BatchItem, ObsHandle, ObsStatus, db_to_mul, mul_to_db, response_data},
    *,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_stream::{
    StreamExt as _,
    wrappers::{BroadcastStream, errors::BroadcastStreamRecvError},
};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

const EVENT_CAPACITY: usize = 256;
const MAX_LOGIN_FAILURES: u32 = 5;
const LOGIN_BLOCK: Duration = Duration::from_secs(30);
const SESSION_IDLE_TTL_SECS: u64 = 2 * 60 * 60;
const SESSION_ABSOLUTE_TTL_SECS: u64 = 12 * 60 * 60;
const STATUS_POLL: Duration = Duration::from_secs(3);
const RAW_OBS_ALLOWLIST: &[&str] = &["GetStreamStatus", "GetRecordStatus"];
/// Как часто уровни звука уходят в панель. OBS считает их ~50 раз в секунду;
/// четырёх обновлений хватает, чтобы видеть, что звук идёт.
const LEVELS_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
struct AppState {
    cfg: Arc<HostConfig>,
    obs: ObsHandle,
    http: reqwest::Client,
    feed: DonationFeed,
    events: broadcast::Sender<Value>,
    /// Токен сессии держим в памяти: расшифровывать DPAPI-файл на каждый
    /// запрос панели — лишний диск и лишняя криптография.
    session: Arc<RwLock<Option<StoredSession>>>,
    /// Одноразовый state для OAuth DonationAlerts (защита от подмены кода).
    oauth_state: Arc<RwLock<Option<String>>>,
    login_guards: Arc<Mutex<HashMap<IpAddr, LoginGuard>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    token: String,
    created_at: u64,
    last_seen: u64,
}

impl StoredSession {
    fn new(token: String, now: u64) -> Self {
        Self {
            token,
            created_at: now,
            last_seen: now,
        }
    }

    fn expired(&self, now: u64) -> bool {
        now.saturating_sub(self.created_at) > SESSION_ABSOLUTE_TTL_SECS
            || now.saturating_sub(self.last_seen) > SESSION_IDLE_TTL_SECS
    }

    fn touch(&mut self, now: u64) {
        self.last_seen = now;
    }
}

#[derive(Default)]
struct LoginGuard {
    failures: u32,
    blocked_until: Option<Instant>,
}

impl LoginGuard {
    fn blocked_for(&self) -> Option<Duration> {
        let until = self.blocked_until?;
        until.checked_duration_since(Instant::now())
    }
    fn record_failure(&mut self) {
        self.failures += 1;
        if self.failures >= MAX_LOGIN_FAILURES {
            self.blocked_until = Some(Instant::now() + LOGIN_BLOCK);
            self.failures = 0;
        }
    }
    fn record_success(&mut self) {
        self.failures = 0;
        self.blocked_until = None;
    }
}

type ApiResult<T> = Result<Json<T>, Response>;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_session_secret() -> Result<Option<StoredSession>> {
    let Some(raw) = load_secret("session_token")? else {
        return Ok(None);
    };
    match serde_json::from_str::<StoredSession>(&raw) {
        Ok(session) if !session.expired(now_unix()) => Ok(Some(session)),
        Ok(_) => {
            delete_secret("session_token").ok();
            Ok(None)
        }
        Err(_) => {
            // Old builds stored a bare token without timestamps. Do not keep it valid forever.
            delete_secret("session_token").ok();
            Ok(None)
        }
    }
}

fn save_session_secret(session: &StoredSession) -> Result<()> {
    save_secret("session_token", &serde_json::to_string(session)?)
}

fn load_runtime_donationalerts_secret(mut cfg: HostConfig) -> Result<HostConfig> {
    let json_secret = cfg.donationalerts.client_secret.trim().to_string();
    if !json_secret.is_empty() {
        save_secret("donationalerts_client_secret", &json_secret)?;
        cfg.donationalerts.client_secret.clear();
        save_json(&config_dir().join("host.json"), &cfg)?;
        cfg.donationalerts.client_secret = json_secret;
        info!("DonationAlerts client_secret migrated from host.json to DPAPI secret store");
        return Ok(cfg);
    }
    if let Some(secret) = load_secret("donationalerts_client_secret")? {
        cfg.donationalerts.client_secret = secret;
    }
    Ok(cfg)
}

fn raw_obs_allowed(request_type: &str) -> bool {
    RAW_OBS_ALLOWLIST.contains(&request_type)
}

/// Два воркера вместо «по числу ядер».
///
/// Агент почти всё время ждёт сокет: нагрузка на ввод-вывод, а не на счёт.
/// По умолчанию tokio поднял бы воркер на каждое ядро, и на 16-ядерной машине
/// это 16 простаивающих потоков со своими стеками. Машина актёра занята
/// кодированием видео, и такты стоит оставить OBS, а не нам. Двух хватает,
/// чтобы обработка запроса не блокировала фоновые задачи.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    ensure_dirs()?;
    let _log_guard = init_logging()?;

    let mut cfg = load_runtime_donationalerts_secret(load_host_config()?)?;
    let obs_password = load_secret("obs_websocket_password")?
        .unwrap_or_else(|| cfg.obs_websocket_password.clone());
    // Пароль не должен остаться в памяти конфига, который сериализуется в ответы.
    cfg.obs_websocket_password.clear();
    let cfg = Arc::new(cfg);

    // Автозапуск: агент стартует при входе в Windows, поэтому сам поднимает OBS.
    if cfg.auto_start_obs {
        match start_obs_if_needed(&cfg.obs_path) {
            Ok(true) => info!("OBS запущен агентом"),
            Ok(false) => info!("OBS уже работает"),
            Err(e) => warn!("OBS не запущен: {e:#}"),
        }
    }

    let (events, _) = broadcast::channel(EVENT_CAPACITY);
    let obs = ObsHandle::spawn(
        &cfg.obs_websocket_host,
        cfg.obs_websocket_port,
        obs_password,
    );
    let feed = DonationFeed::new(events.clone());

    let state = AppState {
        cfg: cfg.clone(),
        obs: obs.clone(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?,
        feed: feed.clone(),
        events: events.clone(),
        session: Arc::new(RwLock::new(load_session_secret()?)),
        oauth_state: Arc::new(RwLock::new(None)),
        login_guards: Arc::new(Mutex::new(HashMap::new())),
    };

    forward_obs_events(obs.clone(), events.clone());
    watch_obs_status(obs.clone(), events.clone());
    watch_stream_health(obs.clone(), events.clone());
    if cfg.donationalerts.enabled && cfg.donationalerts.oauth_enabled {
        donationalerts::spawn(cfg.donationalerts.clone(), state.http.clone(), feed.clone());
    }
    if load_secret("donationalerts_widget_url")?.is_some() {
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = reconcile_donationalerts(&s).await {
                error!("DonationAlerts reconcile при старте не удался: {e:#}");
            }
        });
    }

    let app = Router::new()
        .route("/api/public/ping", get(public_ping))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/events", get(sse_events))
        .route("/api/health", get(health))
        .route("/api/obs", get(obs_status))
        .route("/api/obs/launch", post(obs_launch))
        .route("/api/obs/request", post(obs_request))
        .route("/api/obs/scenes", get(obs_scenes))
        .route("/api/obs/scenes/current", post(obs_set_scene))
        .route("/api/obs/sources", get(obs_sources))
        .route("/api/obs/source/visibility", post(obs_source_visibility))
        .route("/api/obs/audio", get(obs_audio))
        .route("/api/obs/audio/mute", post(obs_audio_mute))
        .route("/api/obs/audio/volume", post(obs_audio_volume))
        .route("/api/obs/stream/start", post(obs_stream_start))
        .route("/api/obs/stream/stop", post(obs_stream_stop))
        .route("/api/obs/record/start", post(obs_record_start))
        .route("/api/obs/record/stop", post(obs_record_stop))
        .route("/api/obs/record/pause", post(obs_record_pause))
        .route("/api/obs/record/resume", post(obs_record_resume))
        .route("/api/obs/stats", get(obs_stats))
        .route("/api/obs/preview", get(obs_preview))
        .route("/api/obs/studio", get(obs_studio).post(obs_studio_set))
        .route("/api/obs/studio/preview", post(obs_studio_preview))
        .route("/api/obs/studio/transition", post(obs_studio_transition))
        .route("/api/obs/virtualcam", get(obs_virtualcam))
        .route("/api/obs/virtualcam/start", post(obs_virtualcam_start))
        .route("/api/obs/virtualcam/stop", post(obs_virtualcam_stop))
        .route("/api/obs/replay", get(obs_replay))
        .route("/api/obs/replay/start", post(obs_replay_start))
        .route("/api/obs/replay/stop", post(obs_replay_stop))
        .route("/api/obs/replay/save", post(obs_replay_save))
        .route("/api/obs/profiles", get(obs_profiles).post(obs_profile_set))
        .route(
            "/api/obs/collections",
            get(obs_collections).post(obs_collection_set),
        )
        .route(
            "/api/obs/transitions",
            get(obs_transitions).post(obs_transition_set),
        )
        .route("/api/donationalerts/status", get(da_status))
        .route("/api/donationalerts/recent", get(da_recent))
        .route("/api/donationalerts/reconcile", post(da_reconcile))
        .route("/api/donationalerts/widget-url", post(da_widget_url))
        .route(
            "/api/donationalerts/widget/refresh",
            post(da_widget_refresh),
        )
        .route("/api/donationalerts/widget/mute", post(da_widget_mute))
        .route("/api/donationalerts/widget/volume", post(da_widget_volume))
        .route("/api/donationalerts/oauth/start", post(da_oauth_start))
        .route("/api/donationalerts/oauth/callback", get(da_oauth_callback))
        .route("/api/twitch/status", get(twitch_status))
        .route("/api/twitch/device/start", post(twitch_device_start))
        .route("/api/twitch/device/check", post(twitch_device_check))
        .route(
            "/api/twitch/channel",
            get(twitch_channel_get).post(twitch_channel_modify),
        )
        .route("/api/twitch/marker", post(twitch_marker))
        .fallback_service(ServeDir::new(web_dir()).append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    serve(app, &cfg).await
}

/// Слушаем и Tailscale-адрес, и localhost.
///
/// Localhost нужен не для удобства: `redirect_uri` OAuth DonationAlerts
/// указывает на 127.0.0.1, и без этого слушателя браузер актёра не смог бы
/// доставить authorization code агенту. На безопасность это не влияет —
/// петлевой интерфейс недоступен извне.
async fn serve(app: Router, cfg: &HostConfig) -> Result<()> {
    let mut addrs = vec![SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        cfg.web_port,
    )];
    if cfg.listen_mode == "tailscale_only" {
        match tailscale_ip() {
            Some(ip) => addrs.push(SocketAddr::new(ip, cfg.web_port)),
            None => warn!(
                "Tailscale IP не определён — панель доступна только на localhost. \
                 Проверьте, что Tailscale установлен и выполнен вход."
            ),
        }
    }

    let mut servers = Vec::new();
    for addr in addrs {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                info!("Host Agent слушает http://{}", addr);
                println!("Remote Stream Control Host Agent: http://{addr}");
                let app = app.clone();
                servers.push(tokio::spawn(async move {
                    let service = app.into_make_service_with_connect_info::<SocketAddr>();
                    if let Err(e) = axum::serve(listener, service).await {
                        error!("Сервер на {addr} остановлен: {e}");
                    }
                }));
            }
            Err(e) => warn!("Не удалось занять {addr}: {e}"),
        }
    }
    if servers.is_empty() {
        return Err(anyhow!(
            "Не удалось занять ни один адрес на порту {}. Возможно, агент уже запущен.",
            cfg.web_port
        ));
    }
    for s in servers {
        let _ = s.await;
    }
    Ok(())
}

fn init_logging() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let file = tracing_appender::rolling::never(logs_dir(), "host.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    // Guard должен жить всё время работы процесса, иначе логи перестанут писаться.
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .ok();
    Ok(guard)
}

/// События OBS уходят в общий поток панели.
///
/// Уровни звука обрабатываются отдельно: OBS шлёт их около 50 раз в секунду, и
/// пересылать это в браузер как есть — значит завалить и канал, и отрисовку.
/// Отдаём сводку не чаще LEVELS_INTERVAL, чего глазу достаточно.
fn forward_obs_events(obs: ObsHandle, events: broadcast::Sender<Value>) {
    tokio::spawn(async move {
        let mut rx = obs.subscribe();
        let mut levels_sent = Instant::now() - LEVELS_INTERVAL;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let kind = event.get("eventType").and_then(Value::as_str).unwrap_or("");
                    if kind == "InputVolumeMeters" {
                        if levels_sent.elapsed() < LEVELS_INTERVAL {
                            continue;
                        }
                        levels_sent = Instant::now();
                        let levels: serde_json::Map<String, Value> =
                            obs::meter_levels(event.get("eventData").unwrap_or(&Value::Null))
                                .into_iter()
                                .map(|(name, db)| (name, json!((db * 10.0).round() / 10.0)))
                                .collect();
                        if !levels.is_empty() {
                            let _ = events.send(json!({"type": "levels", "levels": levels}));
                        }
                        continue;
                    }
                    let _ = events.send(json!({"type": "obs", "event": event}));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Потерянные события молчаливо пропускать нельзя: панель
                    // продолжила бы показывать состояние, которое уже неверно,
                    // и выглядела бы при этом исправной. Просим её перечитать всё.
                    warn!("Панель отстала на {n} событий OBS, требуется полное обновление");
                    let _ = events.send(json!({
                        "type": "resync_required",
                        "lost": n,
                    }));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// OBS не сообщает о собственном обрыве событием, поэтому следим за статусом
/// соединения сами и шлём в панель только изменения.
fn watch_obs_status(obs: ObsHandle, events: broadcast::Sender<Value>) {
    tokio::spawn(async move {
        let mut last: Option<bool> = None;
        loop {
            let status = obs.status().await;
            if last != Some(status.connected) {
                last = Some(status.connected);
                let _ = events.send(json!({"type": "obs_status", "status": status}));
            }
            tokio::time::sleep(STATUS_POLL).await;
        }
    });
}

/// Следит за потерей кадров и местом на диске.
///
/// Живёт в агенте, а не в панели, намеренно: панель может быть закрыта, а
/// авария случиться. Здесь она хотя бы попадёт в лог, и владелец увидит её
/// в журнале, когда откроет панель.
fn watch_stream_health(obs: ObsHandle, events: broadcast::Sender<Value>) {
    tokio::spawn(async move {
        let mut watch = HealthWatch::new();
        loop {
            tokio::time::sleep(health::SAMPLE_INTERVAL).await;
            if !obs.is_connected().await {
                continue;
            }
            // Оба показателя за один round-trip.
            let Ok(results) = obs
                .batch(vec![
                    BatchItem::new("GetStats", json!({})),
                    BatchItem::new("GetStreamStatus", json!({})),
                ])
                .await
            else {
                continue;
            };
            let stats = results.first().and_then(response_data);
            let stream = results.get(1).and_then(response_data);
            let number = |v: Option<&Value>, key: &str| {
                v.and_then(|d| d.get(key))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
            };

            let sample = health::Sample {
                total_frames: number(stream, "outputTotalFrames"),
                skipped_frames: number(stream, "outputSkippedFrames"),
                free_disk_mb: number(stats, "availableDiskSpace"),
                streaming: stream
                    .and_then(|d| d.get("outputActive"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };

            for alert in watch.observe(sample) {
                let text = alert.message();
                if alert.is_urgent() {
                    warn!("{}", text);
                } else {
                    info!("{}", text);
                }
                let _ = events.send(json!({
                    "type": "alert",
                    "alert": alert,
                    "message": text,
                    "urgent": alert.is_urgent(),
                }));
            }
        }
    });
}

async fn public_ping() -> Json<Value> {
    Json(json!({"ok": true, "app": APP_NAME}))
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
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
fn secret_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

async fn is_auth(st: &AppState, headers: &HeaderMap) -> bool {
    let Some(presented) = cookie(headers, "rsc_session") else {
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

async fn require_auth(st: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if is_auth(st, headers).await {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "Требуется pairing-код Remote Stream Control."}})),
        )
            .into_response())
    }
}

async fn auth_status(State(st): State<AppState>, headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "paired": load_secret("pairing_secret").ok().flatten().is_some(),
        "authenticated": is_auth(&st, &headers).await,
    }))
}

async fn auth_login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> Response {
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
            "rsc_session={}; Path=/; HttpOnly; SameSite=Lax",
            session.token
        ))
        .expect("токен из base64 всегда валиден для заголовка"),
    );
    resp
}

async fn auth_logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = require_auth(&st, &headers).await {
        return resp;
    }
    *st.session.write().await = None;
    let _ = delete_secret("session_token");
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_static("rsc_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    resp
}

/// Живой поток событий: OBS, статус соединения, донаты.
async fn sse_events(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, Response> {
    require_auth(&st, &headers).await?;
    let stream = BroadcastStream::new(st.events.subscribe()).filter_map(|msg| {
        let value = match msg {
            Ok(value) => value,
            // У каждого клиента своя очередь. Если браузер не успел её
            // вычитать, события пропали именно для него — молчать об этом
            // нельзя, иначе панель покажет устаревшее состояние как свежее.
            Err(BroadcastStreamRecvError::Lagged(lost)) => {
                warn!("Клиент панели отстал на {lost} событий");
                json!({"type": "resync_required", "lost": lost})
            }
        };
        Event::default().json_data(&value).ok().map(Ok)
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn err(msg: &str, e: anyhow::Error) -> Response {
    error!("{}: {:#}", msg, e);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": {"message": msg, "detail": e.to_string()}})),
    )
        .into_response()
}
fn bad(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": {"message": msg}})),
    )
        .into_response()
}

async fn health(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let obs = st.obs.status().await;
    let ready = obs.connected;
    Ok(Json(json!({
        "tailscale": tailscale_ip().map(|i| i.to_string()).unwrap_or_else(|| "not_detected".into()),
        "tailscale_running": tailscale_running(),
        "host_agent": "ok",
        "autostart": autostart_registered(),
        "obs": obs,
        "obs_process_running": obs_is_running(),
        "obs_crashed_last_run": obs_crashed_last_run(),
        "donationalerts": donationalerts_status_value(&st).await,
        "twitch": twitch_status_value(&st).await,
        "ready_to_stream": ready,
    })))
}

async fn obs_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<ObsStatus> {
    require_auth(&st, &headers).await?;
    Ok(Json(st.obs.status().await))
}

/// Запуск OBS на машине актёра по команде из панели.
///
/// Агент поднимает OBS при входе в Windows, но актёр может закрыть его руками.
/// Без этой ручки владелец остался бы без управления до следующей перезагрузки
/// у актёра, а весь смысл проекта в том, что актёр ничего не делает.
async fn obs_launch(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    match start_obs_if_needed(&st.cfg.obs_path) {
        Ok(true) => {
            // Сбрасываем паузу переподключения, иначе панель ждала бы
            // до 15 секунд, хотя OBS уже поднимается.
            st.obs.reconnect_now();
            Ok(Json(
                json!({"started": true, "message": "OBS запускается. Связь появится через несколько секунд."}),
            ))
        }
        Ok(false) => Ok(Json(
            json!({"started": false, "message": "OBS уже запущен."}),
        )),
        Err(e) => Err(err("Не удалось запустить OBS", e)),
    }
}

async fn obs_request(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let rt = body
        .get("requestType")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("requestType обязателен"))?;
    if !raw_obs_allowed(rt) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": {"message": "Эта OBS-команда не разрешена через raw endpoint. Используйте специализированную API-ручку Remote Stream Control."}})),
        )
            .into_response());
    }
    let data = body
        .get("requestData")
        .cloned()
        .unwrap_or_else(|| json!({}));
    st.obs
        .request(rt, data)
        .await
        .map(Json)
        .map_err(|e| err("Команда OBS не выполнена", e))
}

async fn obs_scenes(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    st.obs
        .request("GetSceneList", json!({}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось получить сцены OBS", e))
}

async fn obs_set_scene(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene = body
        .get("sceneName")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("sceneName обязателен"))?;
    st.obs
        .request("SetCurrentProgramScene", json!({"sceneName": scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось переключить сцену", e))
}

async fn obs_sources(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene = q.get("scene").ok_or_else(|| bad("scene обязателен"))?;
    st.obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось получить источники сцены", e))
}

async fn obs_source_visibility(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene = body
        .get("sceneName")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("sceneName обязателен"))?;
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad("enabled обязателен"))?;
    let id = if let Some(id) = body.get("sceneItemId").and_then(Value::as_i64) {
        id
    } else {
        let source = body
            .get("sourceName")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("sourceName или sceneItemId обязателен"))?;
        find_scene_item_id(&st.obs, scene, source)
            .await
            .map_err(|e| err("Источник не найден", e))?
    };
    st.obs
        .request(
            "SetSceneItemEnabled",
            json!({"sceneName": scene, "sceneItemId": id, "sceneItemEnabled": enabled}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить видимость источника", e))
}

async fn find_scene_item_id(obs: &ObsHandle, scene: &str, source: &str) -> Result<i64> {
    let list = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    list.get("sceneItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|it| it.get("sourceName").and_then(Value::as_str) == Some(source))
        .and_then(|it| it.get("sceneItemId").and_then(Value::as_i64))
        .ok_or_else(|| anyhow!("{source} отсутствует в {scene}"))
}

/// Помечает входы ролью, которую им назначил сам OBS.
///
/// Раньше микрофон угадывался по названию, что ломалось, стоило актёру
/// переименовать источник. OBS знает роли точно: desktop1/2 — звук системы,
/// mic1..4 — микрофоны.
fn special_input_roles(special: &Value) -> HashMap<String, &'static str> {
    let mut roles = HashMap::new();
    for (key, role) in [
        ("desktop1", "desktop"),
        ("desktop2", "desktop"),
        ("mic1", "mic"),
        ("mic2", "mic"),
        ("mic3", "mic"),
        ("mic4", "mic"),
    ] {
        if let Some(name) = special.get(key).and_then(Value::as_str)
            && !name.is_empty()
        {
            roles.insert(name.to_string(), role);
        }
    }
    roles
}

/// Громкость и mute всех входов за два round-trip вместо 2N.
async fn obs_audio(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    // Список входов и их роли — за один round-trip.
    let head = st
        .obs
        .batch(vec![
            BatchItem::new("GetInputList", json!({})),
            BatchItem::new("GetSpecialInputs", json!({})),
        ])
        .await
        .map_err(|e| err("Не удалось получить список источников", e))?;
    let inputs = head
        .first()
        .and_then(response_data)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let roles = head
        .get(1)
        .and_then(response_data)
        .map(special_input_roles)
        .unwrap_or_default();

    let names: Vec<(String, Value)> = inputs
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let name = i.get("inputName").and_then(Value::as_str)?.to_string();
            Some((name, i.get("inputKind").cloned().unwrap_or(Value::Null)))
        })
        .collect();

    // Свежая установка OBS без источников — обычное состояние на первом
    // запуске у актёра. Пустой RequestBatch отправлять незачем.
    if names.is_empty() {
        return Ok(Json(json!({"audio": []})));
    }

    let mut batch = Vec::with_capacity(names.len() * 2);
    for (name, _) in &names {
        batch.push(BatchItem::new("GetInputVolume", json!({"inputName": name})));
        batch.push(BatchItem::new("GetInputMute", json!({"inputName": name})));
    }
    let results = st
        .obs
        .batch(batch)
        .await
        .map_err(|e| err("Не удалось получить состояние аудио", e))?;

    let mut rows = Vec::new();
    for (i, (name, kind)) in names.iter().enumerate() {
        let volume = results.get(i * 2).and_then(response_data);
        let mute = results.get(i * 2 + 1).and_then(response_data);
        // Источники без аудио на эти запросы отвечают ошибкой — пропускаем их.
        if volume.is_none() && mute.is_none() {
            continue;
        }
        let db = volume
            .and_then(|v| v.get("inputVolumeDb"))
            .and_then(Value::as_f64)
            .or_else(|| {
                volume
                    .and_then(|v| v.get("inputVolumeMul"))
                    .and_then(Value::as_f64)
                    .map(mul_to_db)
            })
            .unwrap_or(0.0);
        rows.push(json!({
            "inputName": name,
            "inputKind": kind,
            "role": roles.get(name.as_str()),
            "muted": mute.and_then(|m| m.get("inputMuted")).cloned().unwrap_or(Value::Null),
            "volumeDb": db,
            "volumeMul": volume.and_then(|v| v.get("inputVolumeMul")).cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(Json(json!({"audio": rows})))
}

async fn obs_audio_mute(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let name = body
        .get("inputName")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("inputName обязателен"))?;
    let muted = body
        .get("muted")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad("muted обязателен"))?;
    st.obs
        .request(
            "SetInputMute",
            json!({"inputName": name, "inputMuted": muted}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить mute", e))
}

async fn obs_audio_volume(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let name = body
        .get("inputName")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("inputName обязателен"))?;
    let db = body
        .get("volumeDb")
        .and_then(Value::as_f64)
        .ok_or_else(|| bad("volumeDb обязателен"))?;
    st.obs
        .request(
            "SetInputVolume",
            json!({"inputName": name, "inputVolumeMul": db_to_mul(db)}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить громкость", e))
}

macro_rules! simple_obs {
    ($fn:ident, $req:literal, $msg:literal) => {
        async fn $fn(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
            require_auth(&st, &headers).await?;
            st.obs
                .request($req, json!({}))
                .await
                .map(Json)
                .map_err(|e| err($msg, e))
        }
    };
}
simple_obs!(obs_stream_start, "StartStream", "Не удалось начать эфир");
simple_obs!(obs_stream_stop, "StopStream", "Не удалось остановить эфир");
simple_obs!(obs_record_start, "StartRecord", "Не удалось начать запись");
simple_obs!(
    obs_record_stop,
    "StopRecord",
    "Не удалось остановить запись"
);
simple_obs!(
    obs_record_pause,
    "PauseRecord",
    "Не удалось поставить запись на паузу"
);
simple_obs!(
    obs_record_resume,
    "ResumeRecord",
    "Не удалось продолжить запись"
);
simple_obs!(obs_stats, "GetStats", "Не удалось получить статистику OBS");
simple_obs!(
    obs_studio_transition,
    "TriggerStudioModeTransition",
    "Не удалось выполнить переход"
);
simple_obs!(
    obs_virtualcam,
    "GetVirtualCamStatus",
    "Не удалось узнать состояние виртуальной камеры"
);
simple_obs!(
    obs_virtualcam_start,
    "StartVirtualCam",
    "Не удалось включить виртуальную камеру"
);
simple_obs!(
    obs_virtualcam_stop,
    "StopVirtualCam",
    "Не удалось выключить виртуальную камеру"
);
/// Состояние буфера повтора.
///
/// Если буфер не включён в настройках OBS, запрос отвечает ошибкой. Это не
/// поломка, а обычное положение дел, поэтому отдаём его как состояние —
/// иначе панель пугала бы владельца красной ошибкой на пустом месте.
async fn obs_replay(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    match st.obs.request("GetReplayBufferStatus", json!({})).await {
        Ok(v) => Ok(Json(json!({
            "available": true,
            "outputActive": v.get("outputActive").cloned().unwrap_or(Value::Bool(false)),
        }))),
        Err(e) if e.to_string().contains("not available") => Ok(Json(json!({
            "available": false,
            "outputActive": false,
            "message": "Буфер повтора выключен в настройках OBS у актёра \
                        (Настройки → Вывод → Буфер повтора).",
        }))),
        Err(e) => Err(err("Не удалось узнать состояние повтора", e)),
    }
}
simple_obs!(
    obs_replay_start,
    "StartReplayBuffer",
    "Не удалось включить буфер повтора"
);
simple_obs!(
    obs_replay_stop,
    "StopReplayBuffer",
    "Не удалось выключить буфер повтора"
);
simple_obs!(
    obs_replay_save,
    "SaveReplayBuffer",
    "Не удалось сохранить повтор"
);
simple_obs!(
    obs_profiles,
    "GetProfileList",
    "Не удалось получить список профилей"
);
simple_obs!(
    obs_collections,
    "GetSceneCollectionList",
    "Не удалось получить список коллекций сцен"
);
simple_obs!(
    obs_transitions,
    "GetSceneTransitionList",
    "Не удалось получить список переходов"
);

/// Кадр текущей сцены как data-URI.
///
/// Владелец не видит, что происходит у актёра, а актёр может не заметить
/// чёрный экран или зависший источник. Кадр запрашивается по требованию,
/// а не потоком: постоянный видеопоток занял бы канал и процессор актёра.
async fn obs_preview(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let width = q
        .get("width")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(480)
        .clamp(96, 1920);

    let current = st
        .obs
        .request("GetCurrentProgramScene", json!({}))
        .await
        .map_err(|e| err("Не удалось узнать текущую сцену", e))?;
    // Имя поля менялось между версиями obs-websocket 5.x.
    let scene = current
        .get("currentProgramSceneName")
        .or_else(|| current.get("sceneName"))
        .and_then(Value::as_str)
        .ok_or_else(|| bad("OBS не сообщил имя текущей сцены"))?;

    st.obs
        .request(
            "GetSourceScreenshot",
            json!({
                "sourceName": scene,
                "imageFormat": "jpg",
                "imageWidth": width,
                "imageCompressionQuality": 55,
            }),
        )
        .await
        .map(|mut v| {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("sceneName".into(), Value::String(scene.to_string()));
            }
            Json(v)
        })
        .map_err(|e| err("Не удалось получить кадр", e))
}

async fn obs_studio(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let enabled = st
        .obs
        .request("GetStudioModeEnabled", json!({}))
        .await
        .map_err(|e| err("Не удалось узнать состояние Studio Mode", e))?
        .get("studioModeEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Сцена предпросмотра существует только при включённом Studio Mode.
    let preview = if enabled {
        st.obs
            .request("GetCurrentPreviewScene", json!({}))
            .await
            .ok()
            .and_then(|v| {
                v.get("currentPreviewSceneName")
                    .or_else(|| v.get("sceneName"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
    } else {
        None
    };

    Ok(Json(json!({"enabled": enabled, "previewScene": preview})))
}

async fn obs_studio_set(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let enabled = body
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad("enabled обязателен"))?;
    st.obs
        .request(
            "SetStudioModeEnabled",
            json!({"studioModeEnabled": enabled}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось переключить Studio Mode", e))
}

async fn obs_studio_preview(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene = body
        .get("sceneName")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("sceneName обязателен"))?;
    st.obs
        .request("SetCurrentPreviewScene", json!({"sceneName": scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось выбрать сцену предпросмотра", e))
}

/// Переключение профиля, коллекции сцен и перехода — одинаковые по форме
/// запросы, отличаются только именем поля.
macro_rules! obs_setter {
    ($fn:ident, $req:literal, $field:literal, $msg:literal) => {
        async fn $fn(
            State(st): State<AppState>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> ApiResult<Value> {
            require_auth(&st, &headers).await?;
            let value = body
                .get($field)
                .and_then(Value::as_str)
                .ok_or_else(|| bad(concat!($field, " обязателен")))?;
            st.obs
                .request($req, json!({ $field: value }))
                .await
                .map(Json)
                .map_err(|e| err($msg, e))
        }
    };
}
obs_setter!(
    obs_profile_set,
    "SetCurrentProfile",
    "profileName",
    "Не удалось переключить профиль"
);
obs_setter!(
    obs_collection_set,
    "SetCurrentSceneCollection",
    "sceneCollectionName",
    "Не удалось переключить коллекцию сцен"
);
obs_setter!(
    obs_transition_set,
    "SetCurrentSceneTransition",
    "transitionName",
    "Не удалось выбрать переход"
);

async fn da_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(donationalerts_status_value(&st).await))
}

async fn donationalerts_status_value(st: &AppState) -> Value {
    let c = &st.cfg.donationalerts;
    json!({
        "enabled": c.enabled,
        "widget_url_configured": load_secret("donationalerts_widget_url").ok().flatten().is_some(),
        "oauth_configured": !c.client_id.is_empty() && !c.client_secret.is_empty(),
        "tokens_stored": load_secret("donationalerts_tokens").ok().flatten().is_some(),
        "realtime": st.feed.status().await,
        "overlay_scene": c.overlay_scene_name,
        "input_name": c.input_name,
    })
}

async fn da_recent(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(json!({"donations": st.feed.recent().await})))
}

async fn da_widget_url(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let url = body.get("url").and_then(Value::as_str).unwrap_or("").trim();
    // Виджет DonationAlerts всегда отдаётся по https; http означает опечатку
    // или подмену, и в OBS такой источник грузить не стоит.
    if !url.starts_with("https://") {
        return Err(bad(
            "Введите https-ссылку Alerts Widget со страницы DonationAlerts.",
        ));
    }
    save_secret("donationalerts_widget_url", url)
        .map_err(|e| err("Не удалось сохранить DonationAlerts URL", e))?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("URL сохранён, но настроить OBS не удалось", e))
}

async fn da_reconcile(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("Не удалось настроить DonationAlerts в OBS", e))
}

async fn da_widget_refresh(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let name = &st.cfg.donationalerts.input_name;
    let pressed = st
        .obs
        .request(
            "PressInputPropertiesButton",
            json!({"inputName": name, "propertyName": "refreshnocache"}),
        )
        .await;
    // Старые сборки OBS не знают эту кнопку — тогда достаточно переприменить настройки.
    let result = match pressed {
        Ok(v) => Ok(v),
        Err(_) => {
            st.obs
                .request(
                    "SetInputSettings",
                    json!({"inputName": name, "inputSettings": {}, "overlay": true}),
                )
                .await
        }
    };
    result
        .map(Json)
        .map_err(|e| err("Не удалось обновить виджет", e))
}

async fn da_widget_mute(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let muted = body
        .get("muted")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad("muted обязателен"))?;
    st.obs
        .request(
            "SetInputMute",
            json!({"inputName": st.cfg.donationalerts.input_name, "inputMuted": muted}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить mute DonationAlerts", e))
}

async fn da_widget_volume(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let db = body
        .get("volumeDb")
        .and_then(Value::as_f64)
        .ok_or_else(|| bad("volumeDb обязателен"))?;
    st.obs
        .request(
            "SetInputVolume",
            json!({"inputName": st.cfg.donationalerts.input_name, "inputVolumeMul": db_to_mul(db)}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить громкость DonationAlerts", e))
}

/// Приводит OBS к состоянию, в котором донаты видны и слышны в эфире:
/// отдельная сцена-оверлей, browser_source с виджетом, звук в микшере OBS,
/// и оверлей поверх каждой пользовательской сцены.
async fn reconcile_donationalerts(st: &AppState) -> Result<Value> {
    let cfg = &st.cfg.donationalerts;
    let widget_url = load_secret("donationalerts_widget_url")?
        .ok_or_else(|| anyhow!("DonationAlerts Alerts Widget URL ещё не настроен"))?;

    let scene_list = st.obs.request("GetSceneList", json!({})).await?;
    let has_overlay_scene = scene_list
        .get("scenes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|s| s.get("sceneName").and_then(Value::as_str) == Some(&cfg.overlay_scene_name));
    if !has_overlay_scene {
        st.obs
            .request("CreateScene", json!({"sceneName": cfg.overlay_scene_name}))
            .await?;
    }

    let video = st
        .obs
        .request("GetVideoSettings", json!({}))
        .await
        .unwrap_or_else(|_| json!({"baseWidth": 1920, "baseHeight": 1080}));
    let width = video
        .get("baseWidth")
        .and_then(Value::as_i64)
        .unwrap_or(1920);
    let height = video
        .get("baseHeight")
        .and_then(Value::as_i64)
        .unwrap_or(1080);

    // reroute_audio обязателен: без него звук алерта идёт мимо микшера OBS,
    // и зрители его не услышат.
    let settings = json!({
        "url": widget_url,
        "width": width,
        "height": height,
        "reroute_audio": cfg.reroute_audio,
        "shutdown": false,
        "restart_when_active": false,
    });

    let inputs = st.obs.request("GetInputList", json!({})).await?;
    let existing = inputs
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("inputName").and_then(Value::as_str) == Some(&cfg.input_name))
        .cloned();

    // Шаг 1: источник существует и его настройки верны.
    let created = match existing {
        Some(inp) => {
            if inp.get("inputKind").and_then(Value::as_str) != Some("browser_source") {
                return Err(anyhow!(
                    "Имя {} уже занято источником другого типа. Переименуйте его в OBS или измените input_name в config/host.json.",
                    cfg.input_name
                ));
            }
            st.obs
                .request(
                    "SetInputSettings",
                    json!({"inputName": cfg.input_name, "inputSettings": settings, "overlay": true}),
                )
                .await?;
            false
        }
        None => {
            st.obs
                .request(
                    "CreateInput",
                    json!({
                        "sceneName": cfg.overlay_scene_name,
                        "inputName": cfg.input_name,
                        "inputKind": "browser_source",
                        "inputSettings": settings,
                        "sceneItemEnabled": true,
                    }),
                )
                .await?;
            true
        }
    };

    // Шаг 2: источник лежит именно в сцене-оверлее.
    //
    // Существование input проверяется глобально и ничего не говорит о том,
    // есть ли он в RSC_OVERLAYS. Если элемент удалили из сцены руками, прежний
    // код обновлял настройки и рапортовал об успехе, а оверлея в эфире не было.
    let (overlay_item_id, item_restored) =
        ensure_scene_item(&st.obs, &cfg.overlay_scene_name, &cfg.input_name).await?;
    st.obs
        .request(
            "SetSceneItemEnabled",
            json!({
                "sceneName": cfg.overlay_scene_name,
                "sceneItemId": overlay_item_id,
                "sceneItemEnabled": true,
            }),
        )
        .await
        .ok();

    // Шаг 3: звук.
    //
    // Снятие mute выполняем всегда, а не только при создании. Иначе достаточно
    // один раз заглушить источник — и reconcile будет успешно завершаться,
    // оставляя донаты беззвучными. Именно ради слышимости всё и затевалось.
    //
    // Громкость тоже восстанавливаем всегда: reconcile здесь обещает вернуть
    // DonationAlerts в рабочее эфирное состояние после ручной порчи настроек.
    st.obs.batch(vec![
        BatchItem::new(
            "SetInputMute",
            json!({"inputName": cfg.input_name, "inputMuted": false}),
        ),
        BatchItem::new(
            "SetInputVolume",
            json!({"inputName": cfg.input_name, "inputVolumeMul": db_to_mul(cfg.initial_volume_db)}),
        ),
        // Мониторинг выключаем: иначе актёр слышит алерт в наушниках дважды.
        BatchItem::new(
            "SetInputAudioMonitorType",
            json!({"inputName": cfg.input_name, "monitorType": "OBS_MONITORING_TYPE_NONE"}),
        ),
    ])
    .await
    .ok();

    // Шаг 4: оверлей поверх каждой пользовательской сцены.
    let mut added = 0;
    let mut present = 0;
    if cfg.enforce_overlays {
        let scene_list = st.obs.request("GetSceneList", json!({})).await?;
        let scenes: Vec<String> = scene_list
            .get("scenes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| {
                s.get("sceneName")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|s| s != &cfg.overlay_scene_name)
            .collect();

        for scene in scenes {
            let (id, was_created) =
                ensure_scene_item(&st.obs, &scene, &cfg.overlay_scene_name).await?;
            if was_created {
                added += 1;
            } else {
                present += 1;
            }
            raise_scene_item_to_top(&st.obs, &scene, id).await.ok();
        }
    }

    Ok(json!({
        "ready": true,
        "overlay_scene": cfg.overlay_scene_name,
        "input_name": cfg.input_name,
        "widget_url": "configured",
        "reroute_audio": cfg.reroute_audio,
        "input_created": created,
        "overlay_item_restored": item_restored,
        "scenes_already_had_overlay": present,
        "scenes_added_overlay": added,
    }))
}

/// Возвращает id элемента сцены, создавая его при отсутствии.
/// Второе значение — признак того, что элемент пришлось создать.
async fn ensure_scene_item(obs: &ObsHandle, scene: &str, source: &str) -> Result<(i64, bool)> {
    let items = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    let found = items
        .get("sceneItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("sourceName").and_then(Value::as_str) == Some(source))
        .and_then(|i| i.get("sceneItemId").and_then(Value::as_i64));

    if let Some(id) = found {
        return Ok((id, false));
    }
    let created = obs
        .request(
            "CreateSceneItem",
            json!({"sceneName": scene, "sourceName": source, "sceneItemEnabled": true}),
        )
        .await?;
    let id = created
        .get("sceneItemId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("OBS не вернул id созданного элемента сцены"))?;
    Ok((id, true))
}

/// Индекс верхнего слоя для сцены с указанным числом элементов.
///
/// В obs-websocket индекс 0 — это НИЗ списка источников, а не верх. Прежний код
/// ставил оверлею индекс 0 с комментарием «верхний слой», из-за чего донаты
/// уезжали под захват игры. Ошибка не ловилась ни компилятором, ни CI.
fn top_scene_item_index(item_count: usize) -> i64 {
    item_count.saturating_sub(1) as i64
}

/// Поднимает элемент сцены на самый верх.
async fn raise_scene_item_to_top(obs: &ObsHandle, scene: &str, item_id: i64) -> Result<()> {
    let items = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    let count = items
        .get("sceneItems")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    obs.request(
        "SetSceneItemIndex",
        json!({
            "sceneName": scene,
            "sceneItemId": item_id,
            "sceneItemIndex": top_scene_item_index(count),
        }),
    )
    .await?;
    Ok(())
}

async fn da_oauth_start(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let c = &st.cfg.donationalerts;
    if c.client_id.is_empty() || c.client_secret.is_empty() || c.redirect_uri.is_empty() {
        return Err(bad(
            "Заполните donationalerts.client_id, client_secret и redirect_uri в config/host.json.",
        ));
    }
    let state_token = random_secret_b64(16);
    *st.oauth_state.write().await = Some(state_token.clone());
    let url = format!(
        "https://www.donationalerts.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        urlencoding::encode(&c.client_id),
        urlencoding::encode(&c.redirect_uri),
        urlencoding::encode(&c.oauth_scopes.join(" ")),
        urlencoding::encode(&state_token),
    );
    Ok(Json(json!({"authorize_url": url})))
}

async fn da_oauth_callback(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(code) = q.get("code") else {
        return Html("DonationAlerts: в ответе нет authorization code.").into_response();
    };
    // Проверка state: без неё кто угодно в tailnet мог бы подсунуть чужой код.
    let expected = st.oauth_state.write().await.take();
    match (expected, q.get("state")) {
        (Some(expected), Some(got)) if secret_eq(&expected, got) => {}
        _ => {
            warn!("DonationAlerts OAuth: неверный state, код отклонён");
            return Html(
                "DonationAlerts: проверка state не пройдена. Начните подключение заново из панели.",
            )
            .into_response();
        }
    }

    let c = &st.cfg.donationalerts;
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", c.client_id.as_str()),
        ("client_secret", c.client_secret.as_str()),
        ("redirect_uri", c.redirect_uri.as_str()),
        ("code", code.as_str()),
    ];
    match st
        .http
        .post("https://www.donationalerts.com/oauth/token")
        .form(&form)
        .send()
        .await
    {
        Ok(r) => match r.json::<Value>().await {
            Ok(v) if v.get("access_token").is_some() => {
                if save_secret("donationalerts_tokens", &v.to_string()).is_ok() {
                    Html("DonationAlerts подключён. Можно закрыть эту вкладку.").into_response()
                } else {
                    Html("DonationAlerts: токен получен, но сохранить не удалось.").into_response()
                }
            }
            Ok(_) => Html("DonationAlerts: token endpoint не вернул access_token.").into_response(),
            Err(_) => {
                Html("DonationAlerts: не удалось прочитать ответ token endpoint.").into_response()
            }
        },
        Err(_) => Html("DonationAlerts: token endpoint недоступен.").into_response(),
    }
}

/// Единая обработка ответа Twitch.
///
/// Twitch на 4xx возвращает валидный JSON вида
/// `{"error":"Unauthorized","status":401,"message":"..."}`. Прежний код звал
/// `.json()` сразу, не глядя на статус, и отдавал это тело как успешный ответ —
/// панель радостно сообщала «Маркер создан», хотя не произошло ничего.
/// Для удалённого оператора ложный успех хуже честной ошибки: он узнает правду
/// от зрителей.
async fn twitch_json(response: reqwest::Response, what: &str) -> Result<Value, Response> {
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

async fn twitch_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(twitch_status_value(&st).await))
}

async fn twitch_status_value(st: &AppState) -> Value {
    if st.cfg.twitch.client_id.is_empty() {
        return json!({
            "enabled": st.cfg.twitch.enabled,
            "configured": false,
            "connected": false,
            "message": "Укажите twitch.client_id в config/host.json",
        });
    }
    if load_secret("twitch_tokens").ok().flatten().is_some() {
        return match twitch_user_refreshed(st).await {
            Ok(_) => json!({"enabled": true, "configured": true, "connected": true}),
            Err(e) => json!({
                "enabled": true,
                "configured": true,
                "connected": false,
                "message": format!("{e:#}"),
            }),
        };
    }
    match load_secret("twitch_tokens").ok().flatten() {
        None => json!({"enabled": true, "configured": true, "connected": false}),
        Some(tok) => {
            let v: Value = serde_json::from_str(&tok).unwrap_or_else(|_| json!({}));
            let access = v.get("access_token").and_then(Value::as_str).unwrap_or("");
            match st
                .http
                .get("https://id.twitch.tv/oauth2/validate")
                .bearer_auth(access)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    json!({"enabled": true, "configured": true, "connected": true})
                }
                _ => json!({
                    "enabled": true,
                    "configured": true,
                    "connected": false,
                    "message": "Twitch token устарел или недоступен",
                }),
            }
        }
    }
}

async fn twitch_device_start(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    if st.cfg.twitch.client_id.is_empty() {
        return Err(bad("Укажите twitch.client_id в config/host.json"));
    }
    let scopes = st.cfg.twitch.scopes.join(" ");
    let form = [
        ("client_id", st.cfg.twitch.client_id.as_str()),
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

async fn twitch_device_check(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let dc = load_secret("twitch_device_code")
        .map_err(|e| err("Не удалось прочитать device code", e))?
        .ok_or_else(|| bad("Сначала нажмите «Подключить Twitch»"))?;
    let form = [
        ("client_id", st.cfg.twitch.client_id.as_str()),
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

#[allow(dead_code)]
async fn twitch_user(st: &AppState) -> Result<(String, String)> {
    let tok = load_secret("twitch_tokens")?.ok_or_else(|| anyhow!("Twitch не подключён"))?;
    let v: Value = serde_json::from_str(&tok)?;
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("access_token отсутствует"))?;
    let val: Value = st
        .http
        .get("https://id.twitch.tv/oauth2/validate")
        .bearer_auth(access)
        .send()
        .await?
        .json()
        .await?;
    let uid = val
        .get("user_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Twitch не вернул user_id — вероятно, токен истёк"))?
        .to_string();
    Ok((access.to_string(), uid))
}

async fn twitch_user_refreshed(st: &AppState) -> Result<(String, String)> {
    let tok = load_secret("twitch_tokens")?.ok_or_else(|| anyhow!("Twitch не подключён"))?;
    let v: Value = serde_json::from_str(&tok)?;
    let access = v
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("access_token отсутствует"))?;
    match validate_twitch_access(st, access).await {
        Ok(uid) => Ok((access.to_string(), uid)),
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
            let uid = validate_twitch_access(st, access).await?;
            Ok((access.to_string(), uid))
        }
    }
}

async fn validate_twitch_access(st: &AppState, access: &str) -> Result<String> {
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
    val.get("user_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Twitch не вернул user_id"))
}

async fn refresh_twitch_tokens(st: &AppState, refresh: &str) -> Result<Value> {
    let form = [
        ("client_id", st.cfg.twitch.client_id.as_str()),
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

async fn twitch_channel_get(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let (access, uid) = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let response = st
        .http
        .get("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", uid.as_str())])
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .send()
        .await
        .map_err(|e| err("Twitch недоступен", e.into()))?;
    Ok(Json(
        twitch_json(response, "Чтение параметров канала").await?,
    ))
}

async fn twitch_channel_modify(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let (access, uid) = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let response = st
        .http
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", uid.as_str())])
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .json(&body)
        .send()
        .await
        .map_err(|e| err("Не удалось изменить канал Twitch", e.into()))?;
    // Успех здесь — 204 без тела; twitch_json превратит его в {"ok": true},
    // а любую ошибку — в настоящий код ошибки вместо «ok: false» внутри 200.
    Ok(Json(twitch_json(response, "Изменение канала").await?))
}

async fn twitch_marker(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let (access, uid) = twitch_user_refreshed(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let description = body
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let response = st
        .http
        .post("https://api.twitch.tv/helix/streams/markers")
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .json(&json!({"user_id": uid, "description": description}))
        .send()
        .await
        .map_err(|e| err("Не удалось создать метку Twitch", e.into()))?;
    Ok(Json(twitch_json(response, "Создание метки").await?))
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

    #[test]
    fn cookie_parses_target_among_many() {
        let h = headers_with_cookie("a=1; rsc_session=token-value; b=2");
        assert_eq!(cookie(&h, "rsc_session").as_deref(), Some("token-value"));
    }

    #[test]
    fn cookie_absent_returns_none() {
        let h = headers_with_cookie("other=1");
        assert!(cookie(&h, "rsc_session").is_none());
    }

    #[test]
    fn secret_eq_rejects_different_lengths_and_values() {
        assert!(secret_eq("abcdef", "abcdef"));
        assert!(!secret_eq("abcdef", "abcdeg"));
        assert!(!secret_eq("abcdef", "abcde"));
        assert!(!secret_eq("", "x"));
    }

    #[test]
    fn stored_session_expires_by_idle_and_absolute_ttl() {
        let mut s = StoredSession::new("token".into(), 1_000);
        assert!(!s.expired(1_000 + SESSION_IDLE_TTL_SECS - 1));
        assert!(s.expired(1_000 + SESSION_IDLE_TTL_SECS + 1));

        s.touch(2_000);
        assert!(!s.expired(2_000 + SESSION_IDLE_TTL_SECS - 1));
        assert!(s.expired(1_000 + SESSION_ABSOLUTE_TTL_SECS + 1));
    }

    #[test]
    fn raw_obs_endpoint_is_allowlisted() {
        assert!(raw_obs_allowed("GetStreamStatus"));
        assert!(raw_obs_allowed("GetRecordStatus"));
        assert!(!raw_obs_allowed("SetCurrentProgramScene"));
        assert!(!raw_obs_allowed("SetStreamServiceSettings"));
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
    fn top_index_is_the_last_position_not_zero() {
        // Главный вывод аудита: в obs-websocket индекс 0 — это НИЗ списка.
        // Оверлей с донатами, поставленный на 0, уезжал под захват игры.
        assert_eq!(top_scene_item_index(3), 2);
        assert_eq!(top_scene_item_index(1), 0);
        // Пустая сцена: индекса нет, но паниковать нельзя.
        assert_eq!(top_scene_item_index(0), 0);
    }

    #[test]
    fn special_inputs_are_labelled_by_obs_not_by_name() {
        // Актёр переименовал микрофон во что угодно — роль всё равно известна.
        let roles = special_input_roles(&json!({
            "desktop1": "Звук раб. стола",
            "desktop2": "",
            "mic1": "Петличка Васи",
            "mic2": "Запасной",
            "mic3": "",
            "mic4": "",
        }));
        assert_eq!(roles.get("Петличка Васи"), Some(&"mic"));
        assert_eq!(roles.get("Запасной"), Some(&"mic"));
        assert_eq!(roles.get("Звук раб. стола"), Some(&"desktop"));
        // Пустые слоты не должны попадать в таблицу под пустым именем.
        assert_eq!(roles.len(), 3);
        assert!(!roles.contains_key(""));
    }

    #[test]
    fn special_inputs_tolerate_missing_slots() {
        assert!(special_input_roles(&json!({})).is_empty());
        assert!(special_input_roles(&Value::Null).is_empty());
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
}
