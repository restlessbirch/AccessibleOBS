use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use remote_stream_control::{
    obs::{ObsClient, db_to_mul, mul_to_db},
    *,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    process::Command,
    sync::Arc,
};
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info};

#[derive(Clone)]
struct AppState {
    cfg: HostConfig,
    obs: ObsClient,
    http: reqwest::Client,
    donations: Arc<Mutex<Vec<Value>>>,
}

type ApiResult<T> = Result<Json<T>, Response>;

#[tokio::main]
async fn main() -> Result<()> {
    ensure_dirs()?;
    init_logging()?;
    let mut cfg = load_host_config()?;
    let obs_password = load_secret("obs_websocket_password")?
        .unwrap_or_else(|| cfg.obs_websocket_password.clone());
    cfg.obs_websocket_password.clear();
    let obs = ObsClient::new(
        &cfg.obs_websocket_host,
        cfg.obs_websocket_port,
        obs_password,
    );
    let state = AppState {
        cfg: cfg.clone(),
        obs,
        http: reqwest::Client::new(),
        donations: Arc::new(Mutex::new(Vec::new())),
    };

    let app = Router::new()
        .route("/api/public/ping", get(public_ping))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(auth_login))
        .route("/api/health", get(health))
        .route("/api/obs", get(obs_status))
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
        .with_state(state.clone());

    let addr = bind_addr(&cfg).await;
    info!("Host Agent listening on http://{}", addr);
    println!("Remote Stream Control Host Agent: http://{}", addr);
    if load_secret("donationalerts_widget_url")?.is_some() {
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = reconcile_donationalerts(&s).await {
                error!("DonationAlerts reconcile failed: {}", e);
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn init_logging() -> Result<()> {
    let file = tracing_appender::rolling::never(logs_dir(), "host.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    std::mem::forget(guard);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .ok();
    Ok(())
}
async fn bind_addr(cfg: &HostConfig) -> SocketAddr {
    if cfg.listen_mode == "tailscale_only"
        && let Some(ip) = tailscale_ip()
    {
        return SocketAddr::new(ip, cfg.web_port);
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cfg.web_port)
}
fn tailscale_ip() -> Option<IpAddr> {
    let exe = find_exe("tailscale.exe").or_else(|| {
        let p = std::path::PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
        p.exists().then_some(p)
    })?;
    let out = Command::new(exe).arg("ip").arg("-4").output().ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    txt.lines().next()?.trim().parse().ok()
}

async fn public_ping() -> Json<Value> {
    Json(json!({"ok":true,"app":APP_NAME}))
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
fn is_auth(headers: &HeaderMap) -> bool {
    match (
        cookie(headers, "rsc_session"),
        load_secret("session_token").ok().flatten(),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
#[allow(clippy::result_large_err)]
fn require_auth(headers: &HeaderMap) -> Result<(), Response> {
    if is_auth(headers) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"Требуется pairing-код Remote Stream Control."}})),
        )
            .into_response())
    }
}
async fn auth_status(headers: HeaderMap) -> Json<Value> {
    Json(
        json!({"paired": load_secret("pairing_secret").ok().flatten().is_some(), "authenticated": is_auth(&headers)}),
    )
}
async fn auth_login(Json(body): Json<Value>) -> Response {
    let Some(secret) = body.get("secret").and_then(|v| v.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"message":"Введите pairing-код."}})),
        )
            .into_response();
    };
    match load_secret("pairing_secret") {
        Ok(Some(real)) if secret.trim() == real => {
            let token = load_secret("session_token")
                .ok()
                .flatten()
                .unwrap_or_else(|| random_secret_b64(32));
            if let Err(e) = save_secret("session_token", &token) {
                return err("Не удалось сохранить сессию", e);
            }
            let mut resp = Json(json!({"ok":true})).into_response();
            resp.headers_mut().insert(
                "set-cookie",
                HeaderValue::from_str(&format!(
                    "rsc_session={}; Path=/; HttpOnly; SameSite=Lax",
                    token
                ))
                .unwrap(),
            );
            resp
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"Неверный pairing-код."}})),
        )
            .into_response(),
    }
}
fn err(msg: &str, e: anyhow::Error) -> Response {
    error!("{}: {}", msg, e);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":{"message":msg,"detail":e.to_string()}})),
    )
        .into_response()
}
fn bad(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":{"message":msg}})),
    )
        .into_response()
}

async fn health(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let obs = st.obs.status().await;
    let da = donationalerts_status_value(&st).await;
    let tw = twitch_status_value(&st).await;
    Json(json!({"tailscale": tailscale_ip().map(|i|i.to_string()).unwrap_or_else(||"not_detected".into()), "host_agent":"ok", "obs":obs, "donationalerts":da, "twitch":tw, "ready_to_stream": obs.get("connected").and_then(|v|v.as_bool()).unwrap_or(false)})).pipe(Ok)
}
async fn obs_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    Ok(Json(st.obs.status().await))
}
async fn obs_request(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let rt = body
        .get("requestType")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("requestType обязателен"))?;
    let data = body.get("requestData").cloned().unwrap_or(json!({}));
    st.obs
        .request(rt, data)
        .await
        .map(Json)
        .map_err(|e| err("Команда OBS не выполнена", e))
}
async fn obs_scenes(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
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
    require_auth(&headers)?;
    let scene = body
        .get("sceneName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("sceneName обязателен"))?;
    st.obs
        .request("SetCurrentProgramScene", json!({"sceneName":scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось переключить сцену", e))
}
async fn obs_sources(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let scene = q.get("scene").ok_or_else(|| bad("scene обязателен"))?;
    st.obs
        .request("GetSceneItemList", json!({"sceneName":scene}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось получить источники сцены", e))
}
async fn obs_source_visibility(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let scene = body
        .get("sceneName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("sceneName обязателен"))?;
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| bad("enabled обязателен"))?;
    let id = if let Some(id) = body.get("sceneItemId").and_then(|v| v.as_i64()) {
        id
    } else {
        let source = body
            .get("sourceName")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("sourceName или sceneItemId обязателен"))?;
        find_scene_item_id(&st.obs, scene, source)
            .await
            .map_err(|e| err("Источник не найден", e))?
    };
    st.obs
        .request(
            "SetSceneItemEnabled",
            json!({"sceneName":scene,"sceneItemId":id,"sceneItemEnabled":enabled}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить видимость источника", e))
}
async fn find_scene_item_id(obs: &ObsClient, scene: &str, source: &str) -> Result<i64> {
    let list = obs
        .request("GetSceneItemList", json!({"sceneName":scene}))
        .await?;
    for it in list
        .get("sceneItems")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if it.get("sourceName").and_then(|v| v.as_str()) == Some(source) {
            return it
                .get("sceneItemId")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("sceneItemId отсутствует"));
        }
    }
    Err(anyhow!("{} отсутствует в {}", source, scene))
}
async fn obs_audio(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let inputs = st
        .obs
        .request("GetInputList", json!({}))
        .await
        .map_err(|e| err("Не удалось получить аудиоисточники", e))?;
    let mut rows = Vec::new();
    for inp in inputs
        .get("inputs")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(name) = inp.get("inputName").and_then(|v| v.as_str()) {
            let vol = st
                .obs
                .request("GetInputVolume", json!({"inputName":name}))
                .await
                .unwrap_or(json!({}));
            let mute = st
                .obs
                .request("GetInputMute", json!({"inputName":name}))
                .await
                .unwrap_or(json!({}));
            if vol.get("inputVolumeMul").is_some() || mute.get("inputMuted").is_some() {
                rows.push(json!({"inputName":name,"inputKind":inp.get("inputKind"),"muted":mute.get("inputMuted").cloned().unwrap_or(Value::Null),"volumeDb":vol.get("inputVolumeDb").cloned().unwrap_or_else(|| json!(vol.get("inputVolumeMul").and_then(|v|v.as_f64()).map(mul_to_db).unwrap_or(0.0))),"volumeMul":vol.get("inputVolumeMul").cloned().unwrap_or(Value::Null)}));
            }
        }
    }
    Ok(Json(json!({"audio":rows})))
}
async fn obs_audio_mute(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let name = body
        .get("inputName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("inputName обязателен"))?;
    let muted = body
        .get("muted")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| bad("muted обязателен"))?;
    st.obs
        .request("SetInputMute", json!({"inputName":name,"inputMuted":muted}))
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить mute", e))
}
async fn obs_audio_volume(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let name = body
        .get("inputName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("inputName обязателен"))?;
    let db = body
        .get("volumeDb")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| bad("volumeDb обязателен"))?;
    st.obs
        .request(
            "SetInputVolume",
            json!({"inputName":name,"inputVolumeMul":db_to_mul(db)}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить громкость", e))
}
macro_rules! simple_obs {
    ($fn:ident,$req:literal,$msg:literal) => {
        async fn $fn(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
            require_auth(&headers)?;
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

async fn da_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    Ok(Json(donationalerts_status_value(&st).await))
}
async fn donationalerts_status_value(st: &AppState) -> Value {
    json!({"enabled":st.cfg.donationalerts.enabled,"widget_url_configured":load_secret("donationalerts_widget_url").ok().flatten().is_some(),"oauth_configured": !st.cfg.donationalerts.client_id.is_empty() && !st.cfg.donationalerts.client_secret.is_empty(),"tokens_stored":load_secret("donationalerts_tokens").ok().flatten().is_some(),"overlay_scene":st.cfg.donationalerts.overlay_scene_name,"input_name":st.cfg.donationalerts.input_name})
}
async fn da_recent(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let d = st.donations.lock().await.clone();
    Ok(Json(json!({"donations":d})))
}
async fn da_widget_url(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let url = body
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(bad("Введите корректный URL Alerts Widget."));
    }
    save_secret("donationalerts_widget_url", url)
        .map_err(|e| err("Не удалось сохранить DonationAlerts URL", e))?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("URL сохранён, но OBS не удалось настроить", e))
}
async fn da_reconcile(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("Не удалось настроить DonationAlerts в OBS", e))
}
async fn da_widget_refresh(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let first = st
        .obs
        .request(
            "PressInputPropertiesButton",
            json!({"inputName":st.cfg.donationalerts.input_name,"propertyName":"refreshnocache"}),
        )
        .await;
    let result = match first { Ok(v)=>Ok(v), Err(_)=>st.obs.request("SetInputSettings", json!({"inputName":st.cfg.donationalerts.input_name,"inputSettings":{},"overlay":true})).await };
    result
        .map(Json)
        .map_err(|e| err("Не удалось обновить виджет", e))
}
async fn da_widget_mute(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let muted = body
        .get("muted")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| bad("muted обязателен"))?;
    st.obs
        .request(
            "SetInputMute",
            json!({"inputName":st.cfg.donationalerts.input_name,"inputMuted":muted}),
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
    require_auth(&headers)?;
    let db = body
        .get("volumeDb")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| bad("volumeDb обязателен"))?;
    st.obs
        .request(
            "SetInputVolume",
            json!({"inputName":st.cfg.donationalerts.input_name,"inputVolumeMul":db_to_mul(db)}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить громкость DonationAlerts", e))
}

async fn reconcile_donationalerts(st: &AppState) -> Result<Value> {
    let cfg = &st.cfg.donationalerts;
    let widget_url = load_secret("donationalerts_widget_url")?
        .ok_or_else(|| anyhow!("DonationAlerts Alerts Widget URL ещё не настроен"))?;
    let scene_list = st.obs.request("GetSceneList", json!({})).await?;
    let scenes = scene_list
        .get("scenes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if !scenes
        .iter()
        .any(|s| s.get("sceneName").and_then(|v| v.as_str()) == Some(&cfg.overlay_scene_name))
    {
        st.obs
            .request("CreateScene", json!({"sceneName":cfg.overlay_scene_name}))
            .await?;
    }
    let video = st
        .obs
        .request("GetVideoSettings", json!({}))
        .await
        .unwrap_or(json!({"baseWidth":1920,"baseHeight":1080}));
    let width = video
        .get("baseWidth")
        .and_then(|v| v.as_i64())
        .unwrap_or(1920);
    let height = video
        .get("baseHeight")
        .and_then(|v| v.as_i64())
        .unwrap_or(1080);
    let settings = json!({"url":widget_url,"width":width,"height":height,"reroute_audio":true,"shutdown":false,"restart_when_active":false});
    let inputs = st.obs.request("GetInputList", json!({})).await?;
    let existing = inputs
        .get("inputs")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .find(|i| i.get("inputName").and_then(|v| v.as_str()) == Some(&cfg.input_name))
        .cloned();
    if let Some(inp) = existing {
        if inp.get("inputKind").and_then(|v| v.as_str()) != Some("browser_source") {
            return Err(anyhow!(
                "Имя {} уже занято источником другого типа. Переименуйте его в OBS или измените config.",
                cfg.input_name
            ));
        }
        st.obs
            .request(
                "SetInputSettings",
                json!({"inputName":cfg.input_name,"inputSettings":settings,"overlay":true}),
            )
            .await?;
    } else {
        st.obs.request("CreateInput", json!({"sceneName":cfg.overlay_scene_name,"inputName":cfg.input_name,"inputKind":"browser_source","inputSettings":settings,"sceneItemEnabled":true})).await?;
        st.obs.request("SetInputVolume", json!({"inputName":cfg.input_name,"inputVolumeMul":db_to_mul(cfg.initial_volume_db)})).await.ok();
        st.obs
            .request(
                "SetInputMute",
                json!({"inputName":cfg.input_name,"inputMuted":false}),
            )
            .await
            .ok();
    }
    st.obs
        .request(
            "SetInputAudioMonitorType",
            json!({"inputName":cfg.input_name,"monitorType":"OBS_MONITORING_TYPE_NONE"}),
        )
        .await
        .ok();
    let scene_list = st.obs.request("GetSceneList", json!({})).await?;
    let mut added = 0;
    let mut present = 0;
    for s in scene_list
        .get("scenes")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(scene) = s.get("sceneName").and_then(|v| v.as_str()) else {
            continue;
        };
        if scene == cfg.overlay_scene_name {
            continue;
        };
        let items = st
            .obs
            .request("GetSceneItemList", json!({"sceneName":scene}))
            .await?;
        let found = items
            .get("sceneItems")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find(|i| {
                i.get("sourceName").and_then(|v| v.as_str()) == Some(&cfg.overlay_scene_name)
            });
        if let Some(item) = found {
            present += 1;
            if let Some(id) = item.get("sceneItemId").and_then(|v| v.as_i64()) {
                st.obs
                    .request(
                        "SetSceneItemIndex",
                        json!({"sceneName":scene,"sceneItemId":id,"sceneItemIndex":0}),
                    )
                    .await
                    .ok();
            }
        } else {
            let r=st.obs.request("CreateSceneItem", json!({"sceneName":scene,"sourceName":cfg.overlay_scene_name,"sceneItemEnabled":true})).await?;
            added += 1;
            if let Some(id) = r.get("sceneItemId").and_then(|v| v.as_i64()) {
                st.obs
                    .request(
                        "SetSceneItemIndex",
                        json!({"sceneName":scene,"sceneItemId":id,"sceneItemIndex":0}),
                    )
                    .await
                    .ok();
            }
        }
    }
    Ok(
        json!({"ready":true,"overlay_scene":cfg.overlay_scene_name,"input_name":cfg.input_name,"widget_url":"configured","reroute_audio":true,"scenes_already_had_overlay":present,"scenes_added_overlay":added}),
    )
}

async fn da_oauth_start(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let c = &st.cfg.donationalerts;
    if c.client_id.is_empty() || c.redirect_uri.is_empty() {
        return Err(bad(
            "Заполните donationalerts.client_id/client_secret/redirect_uri в config/host.json.",
        ));
    }
    let scope = c.oauth_scopes.join(" ");
    let url = format!(
        "https://www.donationalerts.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}",
        urlencoding::encode(&c.client_id),
        urlencoding::encode(&c.redirect_uri),
        urlencoding::encode(&scope)
    );
    Ok(Json(json!({"authorize_url":url})))
}
async fn da_oauth_callback(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(code) = q.get("code") else {
        return Html("DonationAlerts: authorization code missing").into_response();
    };
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
            Ok(v) => {
                if save_secret("donationalerts_tokens", &v.to_string()).is_ok() {
                    Html("DonationAlerts подключён. Можно закрыть эту вкладку.").into_response()
                } else {
                    Html("DonationAlerts token получен, но не сохранён.").into_response()
                }
            }
            Err(_) => {
                Html("DonationAlerts: не удалось прочитать ответ token endpoint.").into_response()
            }
        },
        Err(_) => Html("DonationAlerts: token endpoint недоступен.").into_response(),
    }
}

async fn twitch_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    Ok(Json(twitch_status_value(&st).await))
}
async fn twitch_status_value(st: &AppState) -> Value {
    if st.cfg.twitch.client_id.is_empty() {
        return json!({"enabled":st.cfg.twitch.enabled,"configured":false,"connected":false,"message":"Укажите twitch.client_id в config/host.json"});
    };
    match load_secret("twitch_tokens").ok().flatten() {
        None => json!({"enabled":true,"configured":true,"connected":false}),
        Some(tok) => {
            let v: Value = serde_json::from_str(&tok).unwrap_or(json!({}));
            let access = v.get("access_token").and_then(|v| v.as_str()).unwrap_or("");
            match st
                .http
                .get("https://id.twitch.tv/oauth2/validate")
                .bearer_auth(access)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    json!({"enabled":true,"configured":true,"connected":true})
                }
                _ => {
                    json!({"enabled":true,"configured":true,"connected":false,"message":"Twitch token устарел или недоступен"})
                }
            }
        }
    }
}
async fn twitch_device_start(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    if st.cfg.twitch.client_id.is_empty() {
        return Err(bad("Укажите twitch.client_id в config/host.json"));
    };
    let form = [
        ("client_id", st.cfg.twitch.client_id.as_str()),
        ("scopes", st.cfg.twitch.scopes.join(" ").leak()),
    ];
    let v = st
        .http
        .post("https://id.twitch.tv/oauth2/device")
        .form(&form)
        .send()
        .await
        .map_err(|e| err("Twitch Device Code недоступен", e.into()))?
        .json::<Value>()
        .await
        .map_err(|e| err("Twitch вернул непонятный ответ", e.into()))?;
    save_secret(
        "twitch_device_code",
        v.get("device_code").and_then(|v| v.as_str()).unwrap_or(""),
    )
    .ok();
    Ok(Json(v))
}
async fn twitch_device_check(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let dc = load_secret("twitch_device_code")
        .map_err(|e| err("Device code не найден", e))?
        .ok_or_else(|| bad("Сначала нажмите Подключить Twitch"))?;
    let form = [
        ("client_id", st.cfg.twitch.client_id.as_str()),
        ("device_code", dc.as_str()),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ];
    let v = st
        .http
        .post("https://id.twitch.tv/oauth2/token")
        .form(&form)
        .send()
        .await
        .map_err(|e| err("Не удалось проверить Twitch authorization", e.into()))?
        .json::<Value>()
        .await
        .map_err(|e| err("Twitch вернул непонятный token response", e.into()))?;
    if v.get("access_token").is_some() {
        save_secret("twitch_tokens", &v.to_string())
            .map_err(|e| err("Не удалось сохранить Twitch token", e))?;
        delete_secret("twitch_device_code").ok();
    }
    Ok(Json(v))
}
async fn twitch_user(st: &AppState) -> Result<(String, String)> {
    let tok = load_secret("twitch_tokens")?.ok_or_else(|| anyhow!("Twitch не подключён"))?;
    let v: Value = serde_json::from_str(&tok)?;
    let access = v
        .get("access_token")
        .and_then(|v| v.as_str())
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
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("user_id отсутствует"))?
        .to_string();
    Ok((access.to_string(), uid))
}
async fn twitch_channel_get(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&headers)?;
    let (access, uid) = twitch_user(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let v = st
        .http
        .get("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", uid.as_str())])
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .send()
        .await
        .map_err(|e| err("Twitch channel недоступен", e.into()))?
        .json::<Value>()
        .await
        .map_err(|e| err("Twitch channel response непонятен", e.into()))?;
    Ok(Json(v))
}
async fn twitch_channel_modify(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let (access, uid) = twitch_user(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let r = st
        .http
        .patch("https://api.twitch.tv/helix/channels")
        .query(&[("broadcaster_id", uid.as_str())])
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .json(&body)
        .send()
        .await
        .map_err(|e| err("Не удалось изменить Twitch channel", e.into()))?;
    Ok(Json(
        json!({"ok":r.status().is_success(),"status":r.status().as_u16()}),
    ))
}
async fn twitch_marker(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&headers)?;
    let (access, uid) = twitch_user(&st)
        .await
        .map_err(|e| err("Twitch не подключён", e))?;
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let v = st
        .http
        .post("https://api.twitch.tv/helix/streams/markers")
        .header("Client-Id", &st.cfg.twitch.client_id)
        .bearer_auth(access)
        .json(&json!({"user_id":uid,"description":description}))
        .send()
        .await
        .map_err(|e| err("Не удалось создать Twitch marker", e.into()))?
        .json::<Value>()
        .await
        .map_err(|e| err("Twitch marker response непонятен", e.into()))?;
    Ok(Json(v))
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}
