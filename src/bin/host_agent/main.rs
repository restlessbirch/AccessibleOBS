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
    /// Последние измеренные уровни звука по источникам, dB.
    ///
    /// Нужны проверке готовности: включённый микрофон и звучащий микрофон —
    /// разные вещи, а отошедший кабель OBS показывает как исправный вход.
    levels: Arc<RwLock<HashMap<String, f64>>>,
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

mod auth;
mod donations;
mod twitch;
use auth::*;
use donations::*;
use twitch::*;

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

fn runtime_obs_password(cfg: &HostConfig) -> Result<String> {
    if let Some(secret) = load_secret("obs_websocket_password")? {
        return Ok(secret);
    }
    if !cfg.obs_websocket_password.trim().is_empty() {
        let password = cfg.obs_websocket_password.clone();
        save_secret("obs_websocket_password", &password)?;
        return Ok(password);
    }
    if let Some(password) = existing_obs_websocket_password() {
        save_secret("obs_websocket_password", &password)?;
        info!("OBS WebSocket password imported from existing OBS configuration");
        return Ok(password);
    }
    Ok(String::new())
}

fn raw_obs_allowed(request_type: &str) -> bool {
    RAW_OBS_ALLOWLIST.contains(&request_type)
}

fn arg_present(name: &str) -> bool {
    std::env::args().skip(1).any(|arg| arg == name)
}

fn local_mode_arg_present() -> bool {
    arg_present("--local")
}

fn no_open_arg_present() -> bool {
    arg_present("--no-open")
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
    if local_mode_arg_present() {
        cfg.runtime_mode = RuntimeMode::Local;
    }
    let obs_password = runtime_obs_password(&cfg)?;
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
        levels: Arc::new(RwLock::new(HashMap::new())),
    };

    forward_obs_events(obs.clone(), events.clone(), state.levels.clone());
    watch_obs_status(obs.clone(), events.clone());
    watch_stream_health(obs.clone(), events.clone());
    if cfg.donationalerts.enabled && cfg.donationalerts.oauth_enabled {
        // Не снимок, а способ перечитать: владелец может сменить client_id и
        // client_secret из панели, и воркер обязан узнать об этом до того,
        // как полезет обновлять токен.
        let base = cfg.donationalerts.clone();
        donationalerts::spawn(
            Arc::new(move || resolve_donationalerts_config(&base)),
            state.http.clone(),
            feed.clone(),
        );
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
        .route("/api/runtime", get(runtime_info))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .route("/api/events", get(sse_events))
        .route("/api/health", get(health))
        .route("/api/preflight", get(preflight))
        .route("/api/obs", get(obs_status))
        .route("/api/obs/launch", post(obs_launch))
        .route("/api/obs/request", post(obs_request))
        .route("/api/obs/scenes", get(obs_scenes).post(obs_scene_create))
        .route("/api/obs/scenes/current", post(obs_set_scene))
        .route("/api/obs/input-kinds", get(obs_input_kinds))
        .route("/api/obs/sources", get(obs_sources).post(obs_source_create))
        .route("/api/obs/source/properties", post(obs_source_properties))
        .route(
            "/api/obs/source/settings",
            get(obs_source_settings).post(obs_source_settings_set),
        )
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
        .route(
            "/api/donationalerts/config",
            get(da_config_get).post(da_config_set),
        )
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
        .route(
            "/api/twitch/config",
            get(twitch_config_get).post(twitch_config_set),
        )
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
    if cfg.runtime_mode == RuntimeMode::Remote && cfg.listen_mode == "tailscale_only" {
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
                if cfg.runtime_mode == RuntimeMode::Local
                    && addr.ip().is_loopback()
                    && !no_open_arg_present()
                {
                    let url = format!("http://127.0.0.1:{}", cfg.web_port);
                    if let Err(e) = open::that(&url) {
                        warn!("Не удалось открыть локальную панель {url}: {e:#}");
                    }
                }
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
fn forward_obs_events(
    obs: ObsHandle,
    events: broadcast::Sender<Value>,
    levels_store: Arc<RwLock<HashMap<String, f64>>>,
) {
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
                        let measured =
                            obs::meter_levels(event.get("eventData").unwrap_or(&Value::Null));
                        // Храним последние уровни: проверка готовности спросит
                        // их разом, а не будет ждать очередного события.
                        {
                            let mut store = levels_store.write().await;
                            for (name, db) in &measured {
                                store.insert(name.clone(), *db);
                            }
                        }
                        let levels: serde_json::Map<String, Value> = measured
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

/// Отклик для скриптов запуска. Обязан сообщать режим.
///
/// Раньше отдавался только `ok`, и START_LOCAL.bat не мог отличить локальный
/// агент от удалённого. А удалённый агент прописан в автозагрузке и слушает
/// тот же 127.0.0.1:8787 — то есть в самом обычном случае START_LOCAL видел
/// успешный отклик, решал, что делать нечего, и открывал браузер. Человек
/// вместо локального режима получал удалённый с требованием pairing-кода.
async fn public_ping(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "app": APP_NAME,
        "mode": if st.cfg.runtime_mode == RuntimeMode::Local { "local" } else { "remote" },
    }))
}

async fn runtime_info(State(st): State<AppState>) -> Json<Value> {
    let local = st.cfg.runtime_mode == RuntimeMode::Local;
    Json(json!({
        "mode": if local { "local" } else { "remote" },
        "loopback_only": local,
        "tailscale_required": !local && st.cfg.listen_mode == "tailscale_only",
    }))
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
        // Намеренно НЕ «готов к эфиру»: связь с OBS не означает, что эфир
        // получится. Сцена может быть пустой, микрофон заглушен, сервис
        // вещания не настроен. Обещать готовность по одному признаку —
        // ровно тот ложный успех, которого проект избегает.
        "obs_controllable": ready,
    })))
}

/// Собирает состояние OBS и отвечает, можно ли начинать эфир.
///
/// В отличие от /api/health это не «агент жив», а связная проверка по смыслу:
/// есть ли что показывать, слышно ли оператора, не уедет ли запись в полный
/// диск, не утечёт ли в эфир речь экранного диктора вместе со звуком системы.
async fn preflight(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;

    let obs_status = st.obs.status().await;
    if !obs_status.connected {
        let checks = preflight::evaluate(&preflight::Snapshot::default());
        return Ok(Json(json!({
            "summary": preflight::summary(&checks),
            "checks": checks,
        })));
    }

    // Всё, что можно, — одним пакетом: проверка должна быть быстрой, иначе
    // ею не станут пользоваться перед каждым эфиром.
    let head = st
        .obs
        .batch(vec![
            BatchItem::new("GetCurrentProgramScene", json!({})),
            BatchItem::new("GetInputList", json!({})),
            BatchItem::new("GetSpecialInputs", json!({})),
            BatchItem::new("GetStats", json!({})),
            BatchItem::new("GetStreamStatus", json!({})),
            BatchItem::new("GetRecordStatus", json!({})),
            BatchItem::new("GetStreamServiceSettings", json!({})),
        ])
        .await
        .map_err(|e| err("Не удалось опросить OBS", e))?;
    let at = |i: usize| head.get(i).and_then(response_data);

    let current_scene = at(0).and_then(|v| {
        v.get("currentProgramSceneName")
            .or_else(|| v.get("sceneName"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });

    let roles = at(2).map(special_input_roles).unwrap_or_default();
    // Вид входа нужен наравне с именем: роль desktop OBS присваивает только
    // своим слотам, а обычный «Захват выходного аудиопотока» пишет системный
    // звук без всякой роли — и утащил бы в эфир речь экранного диктора.
    let inputs: Vec<(String, Option<String>)> = at(1)
        .and_then(|v| v.get("inputs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some((
                i.get("inputName").and_then(Value::as_str)?.to_string(),
                i.get("inputKind")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect();

    // Состояние заглушения — отдельным пакетом, по одному запросу на источник.
    let mutes = st
        .obs
        .batch(
            inputs
                .iter()
                .map(|(name, _)| BatchItem::new("GetInputMute", json!({"inputName": name})))
                .collect(),
        )
        .await
        .unwrap_or_default();
    let levels = st.levels.read().await.clone();

    let audio: Vec<preflight::AudioInput> = inputs
        .iter()
        .enumerate()
        .filter_map(|(i, (name, kind))| {
            // Источники без звука на GetInputMute отвечают ошибкой — они
            // к проверке готовности отношения не имеют.
            let muted = mutes
                .get(i)
                .and_then(response_data)?
                .get("inputMuted")
                .and_then(Value::as_bool)?;
            Some(preflight::AudioInput {
                name: name.clone(),
                role: roles.get(name.as_str()).map(|r| (*r).to_string()),
                kind: kind.clone(),
                muted,
                level_db: levels.get(name).copied(),
            })
        })
        .collect();

    let sources = match &current_scene {
        Some(scene) => st
            .obs
            .request("GetSceneItemList", json!({"sceneName": scene}))
            .await
            .ok()
            .and_then(|v| v.get("sceneItems").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|i| {
                Some(preflight::SceneSource {
                    name: i.get("sourceName").and_then(Value::as_str)?.to_string(),
                    enabled: i
                        .get("sceneItemEnabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true),
                    kind: i
                        .get("inputKind")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    // Вложенная сцена и группа картинку дают, но вида входа
                    // не имеют — по одному лишь kind их не отличить от звука.
                    is_scene_or_group: i.get("isGroup").and_then(Value::as_bool) == Some(true)
                        || i.get("sourceType").and_then(Value::as_str)
                            == Some("OBS_SOURCE_TYPE_SCENE"),
                })
            })
            .collect(),
        None => Vec::new(),
    };

    let snapshot = preflight::Snapshot {
        obs_connected: true,
        obs_version: obs_status.obs_version.clone(),
        current_scene,
        sources,
        audio,
        free_disk_mb: at(3)
            .and_then(|v| v.get("availableDiskSpace"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        streaming: at(4)
            .and_then(|v| v.get("outputActive"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        recording: at(5)
            .and_then(|v| v.get("outputActive"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        stream_service_configured: at(6)
            .and_then(|v| v.pointer("/streamServiceSettings/key"))
            .and_then(Value::as_str)
            .is_some_and(|key| !key.trim().is_empty()),
        donation_overlay: donation_overlay_state(&st).await,
        twitch_connected: load_secret("twitch_tokens").ok().flatten().is_some(),
    };

    let checks = preflight::evaluate(&snapshot);
    Ok(Json(json!({
        "summary": preflight::summary(&checks),
        "checks": checks,
    })))
}

/// Состояние оверлея донатов, если DonationAlerts вообще настраивался.
async fn donation_overlay_state(st: &AppState) -> Option<preflight::OverlayState> {
    load_secret("donationalerts_widget_url").ok().flatten()?;
    let verified = verify_donationalerts(st, &st.cfg.donationalerts).await;
    // Берём вердикты как есть. Прежде здесь разбирались тексты проблем
    // подстрокой, и сообщение «не удалось проверить сцену» не содержало
    // слова «нет оверлея» — то есть сбой проверки превращался в «всё на
    // месте», и панель уверенно сообщала, что донаты идут в эфир.
    Some(preflight::OverlayState {
        present_in_scenes: verified.in_overlay_scene,
        on_top: verified.on_top_everywhere,
        audible: verified.audible,
    })
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

async fn obs_scene_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene =
        required_trimmed(&body, "sceneName", "Название сцены обязательно").map_err(|e| bad(&e))?;
    st.obs
        .request("CreateScene", json!({"sceneName": scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось создать сцену", e))
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

async fn obs_input_kinds(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    st.obs
        .request("GetInputKindList", json!({"unversioned": false}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось получить типы источников OBS", e))
}

fn required_trimmed<'a>(body: &'a Value, field: &str, message: &str) -> Result<&'a str, String> {
    body.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| message.to_owned())
}

fn input_kind_exists(kinds: &Value, input_kind: &str) -> bool {
    kinds
        .get("inputKinds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|kind| kind == input_kind)
}

async fn get_input_kinds(st: &AppState) -> Result<Value, Response> {
    st.obs
        .request("GetInputKindList", json!({"unversioned": false}))
        .await
        .map_err(|e| err("Не удалось получить типы источников OBS", e))
}

async fn validate_input_kind(st: &AppState, input_kind: &str) -> Result<(), Response> {
    let kinds = get_input_kinds(st).await?;
    if input_kind_exists(&kinds, input_kind) {
        Ok(())
    } else {
        Err(bad(&format!(
            "OBS не вернул тип источника `{input_kind}`. Обновите список типов и выберите источник из него."
        )))
    }
}

async fn existing_input_kind(obs: &ObsHandle, input_name: &str) -> Result<Option<String>> {
    let inputs = obs.request("GetInputList", json!({})).await?;
    Ok(inputs
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|input| input.get("inputName").and_then(Value::as_str) == Some(input_name))
        .and_then(|input| input.get("inputKind").and_then(Value::as_str))
        .map(str::to_string))
}

async fn obs_source_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene =
        required_trimmed(&body, "sceneName", "sceneName обязателен").map_err(|e| bad(&e))?;
    let source = required_trimmed(&body, "sourceName", "Название источника обязательно")
        .map_err(|e| bad(&e))?;
    let input_kind =
        required_trimmed(&body, "inputKind", "Тип источника обязателен").map_err(|e| bad(&e))?;
    validate_input_kind(&st, input_kind).await?;
    if let Some(existing_kind) = existing_input_kind(&st.obs, source)
        .await
        .map_err(|e| err("Не удалось проверить существующие источники", e))?
    {
        // Тип обязан совпасть. Иначе человек просит «Захват окна», получает
        // молча переиспользованный browser_source с тем же именем и слышит
        // «Существующий источник добавлен». Незрячий мастер настройки такую
        // подмену не увидит и будет искать причину в другом месте.
        if existing_kind != input_kind {
            return Err((
                StatusCode::CONFLICT,
                Json(json!({"error": {"message": format!(
                    "Имя «{source}» уже занято источником типа {existing_kind}, \
                     а вы создаёте {input_kind}. Выберите другое имя или добавьте \
                     существующий источник в сцену.",
                )}})),
            )
                .into_response());
        }
        let (scene_item_id, created_scene_item) =
            ensure_scene_item(&st.obs, scene, source)
                .await
                .map_err(|e| err("Не удалось добавить существующий источник в сцену", e))?;
        return Ok(Json(json!({
            "inputKind": existing_kind,
            "inputName": source,
            "sceneItemId": scene_item_id,
            "createdInput": false,
            "createdSceneItem": created_scene_item,
            "alreadyExisted": true,
        })));
    }
    st.obs
        .request(
            "CreateInput",
            json!({
                "sceneName": scene,
                "inputName": source,
                "inputKind": input_kind,
                "inputSettings": {},
                "sceneItemEnabled": true,
            }),
        )
        .await
        .map(|response| {
            Json(json!({
                "inputKind": input_kind,
                "inputName": source,
                "sceneItemId": response.get("sceneItemId").and_then(Value::as_i64),
                "createdInput": true,
                "createdSceneItem": true,
                "alreadyExisted": false,
                "obs": response,
            }))
        })
        .map_err(|e| err("Не удалось создать источник", e))
}

async fn obs_source_properties(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let source = required_trimmed(&body, "sourceName", "Название источника обязательно")
        .map_err(|e| bad(&e))?;
    st.obs
        .request("OpenInputPropertiesDialog", json!({"inputName": source}))
        .await
        .map(|response| {
            Json(json!({
                "inputName": source,
                "obs": response,
            }))
        })
        .map_err(|e| err("OBS не смог открыть окно свойств источника", e))
}

async fn obs_source_settings(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let source = q
        .get("source")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad("source обязателен"))?;
    st.obs
        .request("GetInputSettings", json!({"inputName": source}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось получить настройки источника", e))
}

async fn obs_source_settings_set(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let source = required_trimmed(&body, "sourceName", "Название источника обязательно")
        .map_err(|e| bad(&e))?;
    let settings = body
        .get("inputSettings")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("inputSettings должен быть объектом"))?;
    st.obs
        .request(
            "SetInputSettings",
            json!({
                "inputName": source,
                "inputSettings": settings,
                "overlay": true,
            }),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось сохранить настройки источника", e))
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

fn scene_audio_names(items: &Value) -> Vec<(String, Value, Value, Value)> {
    items
        .get("sceneItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("sourceName").and_then(Value::as_str)?.to_string();
            Some((
                name,
                item.get("inputKind").cloned().unwrap_or(Value::Null),
                item.get("sceneItemId").cloned().unwrap_or(Value::Null),
                item.get("sceneItemEnabled").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect()
}

async fn scene_source_counts(obs: &ObsHandle) -> Result<HashMap<String, usize>> {
    let scenes = obs.request("GetSceneList", json!({})).await?;
    let names: Vec<String> = scenes
        .get("scenes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|scene| scene.get("sceneName").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let results = obs
        .batch(
            names
                .iter()
                .map(|scene| BatchItem::new("GetSceneItemList", json!({"sceneName": scene})))
                .collect(),
        )
        .await?;
    let mut counts = HashMap::new();
    for result in results.iter().filter_map(response_data) {
        for (name, _, _, _) in scene_audio_names(result) {
            *counts.entry(name).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

/// Громкость и mute входов за два round-trip вместо 2N.
async fn obs_audio(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let scene = q
        .get("scene")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());

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

    let input_kinds: HashMap<String, Value> = inputs
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some((
                i.get("inputName").and_then(Value::as_str)?.to_string(),
                i.get("inputKind").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect();

    let source_counts = if scene.is_some() {
        scene_source_counts(&st.obs).await.unwrap_or_default()
    } else {
        HashMap::new()
    };

    let names: Vec<(String, Value, Value, Value)> = if let Some(scene) = scene {
        let scene_items = st
            .obs
            .request("GetSceneItemList", json!({"sceneName": scene}))
            .await
            .map_err(|e| err("Не удалось получить источники выбранной сцены", e))?;
        scene_audio_names(&scene_items)
            .into_iter()
            .map(|(name, kind, id, enabled)| {
                let kind = if kind.is_null() {
                    input_kinds.get(&name).cloned().unwrap_or(Value::Null)
                } else {
                    kind
                };
                (name, kind, id, enabled)
            })
            .collect()
    } else {
        input_kinds
            .into_iter()
            .map(|(name, kind)| (name, kind, Value::Null, Value::Null))
            .collect()
    };

    // Свежая установка OBS без источников — обычное состояние на первом
    // запуске у актёра. Пустой RequestBatch отправлять незачем.
    if names.is_empty() {
        return Ok(Json(json!({"audio": [], "scene": scene})));
    }

    let mut batch = Vec::with_capacity(names.len() * 2);
    for (name, _, _, _) in &names {
        batch.push(BatchItem::new("GetInputVolume", json!({"inputName": name})));
        batch.push(BatchItem::new("GetInputMute", json!({"inputName": name})));
    }
    let results = st
        .obs
        .batch(batch)
        .await
        .map_err(|e| err("Не удалось получить состояние аудио", e))?;

    let mut rows = Vec::new();
    for (i, (name, kind, scene_item_id, scene_item_enabled)) in names.iter().enumerate() {
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
            "sceneItemId": scene_item_id,
            "sceneItemEnabled": scene_item_enabled,
            "sceneUseCount": source_counts.get(name).copied().unwrap_or(0),
        }));
    }
    Ok(Json(json!({"audio": rows, "scene": scene})))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn input_kind_exists_matches_obs_list() {
        let kinds = json!({"inputKinds": ["browser_source", "dshow_input"]});
        assert!(input_kind_exists(&kinds, "browser_source"));
        assert!(input_kind_exists(&kinds, "dshow_input"));
        assert!(!input_kind_exists(&kinds, "not_from_obs"));
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
}
