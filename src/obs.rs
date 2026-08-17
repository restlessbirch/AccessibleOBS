//! Постоянное соединение с obs-websocket 5.x.
//!
//! Одна фоновая задача держит единственный WebSocket и переподключается сама.
//! Все HTTP-обработчики шлют команды через канал и получают ответ по requestId,
//! поэтому рукопожатие выполняется один раз за соединение, а не на каждый запрос.
//! События OBS раздаются подписчикам через broadcast — панель узнаёт о смене
//! сцены или старте эфира сразу, без опроса.

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, RwLock, broadcast, mpsc, oneshot};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, protocol::CloseFrame},
};
use tracing::{info, warn};
use uuid::Uuid;

// Битовая маска подписок obs-websocket 5.x.
const SUB_GENERAL: u32 = 1 << 0;
const SUB_CONFIG: u32 = 1 << 1;
const SUB_SCENES: u32 = 1 << 2;
const SUB_INPUTS: u32 = 1 << 3;
const SUB_TRANSITIONS: u32 = 1 << 4;
const SUB_OUTPUTS: u32 = 1 << 6;
const SUB_SCENE_ITEMS: u32 = 1 << 7;
const SUB_MEDIA_INPUTS: u32 = 1 << 8;
/// Уровни звука. Отдельная категория, потому что летит ~50 раз в секунду.
const SUB_INPUT_VOLUME_METERS: u32 = 1 << 16;

/// Всё, что нужно панели: сцены, источники, аудио, эфир, запись.
/// Фильтры и вендорские события не подписываем — лишний трафик.
///
/// Уровни звука включены намеренно, несмотря на частоту: до OBS отсюда
/// петлевой интерфейс, где трафик ничего не стоит, а вот дальше в панель
/// они уходят прорежёнными. Без них владелец не может проверить, идёт ли
/// вообще звук в микрофон у актёра — а на слух он этого не сделает.
const EVENT_SUBSCRIPTIONS: u32 = SUB_GENERAL
    | SUB_CONFIG
    | SUB_SCENES
    | SUB_INPUTS
    | SUB_TRANSITIONS
    | SUB_OUTPUTS
    | SUB_SCENE_ITEMS
    | SUB_MEDIA_INPUTS
    | SUB_INPUT_VOLUME_METERS;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Запас перед удалением записи из очереди ожидания.
const PENDING_GRACE: Duration = Duration::from_secs(5);
/// Как часто чистить очередь от просроченных записей.
const PENDING_SWEEP: Duration = Duration::from_secs(15);
/// Предел очереди. Достигается только при потоке команд в мёртвый сокет;
/// без предела память росла бы неограниченно.
const PENDING_MAX: usize = 512;
const RECONNECT_MIN: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(15);
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Пауза между попытками узнать версию OBS, пока он догружается.
const VERSION_RETRY: Duration = Duration::from_secs(2);
/// Хватает на ~10 секунд загрузки OBS; дальше версия просто не показывается.
const VERSION_MAX_TRIES: u8 = 5;
const EVENT_CAPACITY: usize = 256;

/// Один запрос внутри пакета RequestBatch.
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub request_type: String,
    pub request_data: Value,
}
impl BatchItem {
    pub fn new(request_type: impl Into<String>, request_data: Value) -> Self {
        Self {
            request_type: request_type.into(),
            request_data,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ObsStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obs_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

enum Command {
    Request {
        request_type: String,
        request_data: Value,
        reply: oneshot::Sender<Result<Value>>,
    },
    Batch {
        requests: Vec<BatchItem>,
        reply: oneshot::Sender<Result<Vec<Value>>>,
    },
}

enum Reply {
    Single(oneshot::Sender<Result<Value>>),
    Batch(oneshot::Sender<Result<Vec<Value>>>),
}

/// Ожидающий ответа запрос вместе с моментом, после которого он бессмыслен.
///
/// Таймаут вызывающей стороны освобождает только её саму: запись в очереди
/// оставалась навсегда, если OBS завис, но сокет не оборвался. За долгий эфир
/// такие «мёртвые» записи копятся и растят память.
struct Pending {
    reply: Reply,
    deadline: Instant,
}

impl Pending {
    fn new(reply: Reply) -> Self {
        Self {
            reply,
            // Небольшой запас поверх таймаута вызывающей стороны: сначала
            // ответ получает шанс дойти, и только потом запись убирается.
            deadline: Instant::now() + REQUEST_TIMEOUT + PENDING_GRACE,
        }
    }
}

/// Дескриптор соединения. Клонируется свободно, живёт в AppState.
#[derive(Clone)]
pub struct ObsHandle {
    tx: mpsc::Sender<Command>,
    events: broadcast::Sender<Value>,
    status: Arc<RwLock<ObsStatus>>,
    wake: Arc<Notify>,
}

impl ObsHandle {
    pub fn spawn(host: impl Into<String>, port: u16, password: impl Into<String>) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let status = Arc::new(RwLock::new(ObsStatus::default()));
        let wake = Arc::new(Notify::new());
        tokio::spawn(supervise(
            host.into(),
            port,
            password.into(),
            rx,
            events.clone(),
            status.clone(),
            wake.clone(),
        ));
        Self {
            tx,
            events,
            status,
            wake,
        }
    }

    /// Прервать паузу перед следующей попыткой подключения.
    ///
    /// После долгого простоя задержка дорастает до 15 секунд, и это правильно
    /// для случая «OBS просто закрыт». Но когда OBS запускают намеренно, ждать
    /// эти 15 секунд незачем — панель должна ожить сразу.
    pub fn reconnect_now(&self) {
        self.wake.notify_one();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub async fn status(&self) -> ObsStatus {
        self.status.read().await.clone()
    }

    pub async fn is_connected(&self) -> bool {
        self.status.read().await.connected
    }

    pub async fn request(&self, request_type: &str, request_data: Value) -> Result<Value> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Request {
                request_type: request_type.to_string(),
                request_data,
                reply,
            })
            .await
            .map_err(|_| anyhow!("OBS-клиент остановлен"))?;
        await_reply(rx).await?
    }

    /// Один round-trip вместо N запросов. Возвращает по элементу на каждый
    /// запрос пакета, в том же порядке; распаковывать через [`response_data`].
    pub async fn batch(&self, requests: Vec<BatchItem>) -> Result<Vec<Value>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Batch { requests, reply })
            .await
            .map_err(|_| anyhow!("OBS-клиент остановлен"))?;
        await_reply(rx).await?
    }
}

async fn await_reply<T>(rx: oneshot::Receiver<T>) -> Result<T> {
    match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(anyhow!("Соединение с OBS разорвано во время запроса")),
        Err(_) => Err(anyhow!("OBS не ответил за 10 секунд")),
    }
}

/// Достаёт responseData из элемента ответа RequestBatch, если запрос успешен.
pub fn response_data(result: &Value) -> Option<&Value> {
    let ok = result
        .pointer("/requestStatus/result")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ok.then(|| result.get("responseData"))?
}

async fn supervise(
    host: String,
    port: u16,
    password: String,
    mut rx: mpsc::Receiver<Command>,
    events: broadcast::Sender<Value>,
    status: Arc<RwLock<ObsStatus>>,
    wake: Arc<Notify>,
) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let outcome = session(&host, port, &password, &mut rx, &events, &status).await;
        // Успело ли соединение состояться. Читаем до set_disconnected, пока
        // признак ещё не сброшен.
        //
        // Пауза растёт, пока OBS не отвечает, и это правильно. Но копить её
        // между разными соединениями нельзя: если OBS был закрыт полдня, пауза
        // дорастала до предела, а потом, после многочасовой исправной работы,
        // первый же сетевой сбой стоил лишних пятнадцати секунд без управления.
        // Прежде сброс случался только при штатном закрытии, а обрыв с ошибкой
        // прежнее значение сохранял.
        let was_connected = status.read().await.connected;
        match outcome {
            Ok(Disconnect::HandleDropped) => return,
            Ok(Disconnect::Closed) => {
                info!("OBS: соединение закрыто, переподключаюсь");
                set_disconnected(&status, None).await;
            }
            Err(e) => {
                let msg = friendly_obs_error(&format!("{e:#}"));
                warn!("OBS: {}", msg);
                set_disconnected(&status, Some(msg)).await;
            }
        }
        if was_connected {
            backoff = RECONNECT_MIN;
        }
        // Пока ждём переподключения, отвечаем на команды ошибкой сразу,
        // иначе панель висела бы до таймаута на каждой кнопке.
        let deadline = tokio::time::Instant::now() + backoff;
        let mut woken = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = wake.notified() => { woken = true; break; }
                cmd = rx.recv() => match cmd {
                    Some(cmd) => reject(cmd, "OBS не подключён"),
                    None => return,
                },
            }
        }
        backoff = if woken {
            RECONNECT_MIN
        } else {
            (backoff * 2).min(RECONNECT_MAX)
        };
    }
}

async fn set_disconnected(status: &Arc<RwLock<ObsStatus>>, error: Option<String>) {
    let mut guard = status.write().await;
    *guard = ObsStatus {
        connected: false,
        error,
        ..Default::default()
    };
}

fn reject(cmd: Command, reason: &str) {
    match cmd {
        Command::Request { reply, .. } => {
            let _ = reply.send(Err(anyhow!("{reason}")));
        }
        Command::Batch { reply, .. } => {
            let _ = reply.send(Err(anyhow!("{reason}")));
        }
    }
}

enum Disconnect {
    Closed,
    HandleDropped,
}

async fn session(
    host: &str,
    port: u16,
    password: &str,
    rx: &mut mpsc::Receiver<Command>,
    events: &broadcast::Sender<Value>,
    status: &Arc<RwLock<ObsStatus>>,
) -> Result<Disconnect> {
    let url = format!("ws://{host}:{port}");
    let (ws, _) = connect_async(&url)
        .await
        .with_context(|| format!("не удалось подключиться к {url}"))?;
    let (mut sink, mut stream) = ws.split();

    // op0 Hello -> op1 Identify -> op2 Identified
    let hello = read_json(&mut stream, events).await?;
    if hello.get("op").and_then(Value::as_i64) != Some(0) {
        bail!("Неожиданный ответ OBS при подключении");
    }
    let websocket_version = hello
        .pointer("/d/obsWebSocketVersion")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut identify = json!({"rpcVersion": 1, "eventSubscriptions": EVENT_SUBSCRIPTIONS});
    if let Some(auth) = hello.pointer("/d/authentication") {
        if password.is_empty() {
            bail!(
                "OBS WebSocket требует пароль, а он не задан. Запустите AccessibleOBS.exe в режиме актёра — он создаст и сохранит пароль."
            );
        }
        let challenge = auth.get("challenge").and_then(Value::as_str).unwrap_or("");
        let salt = auth.get("salt").and_then(Value::as_str).unwrap_or("");
        identify["authentication"] = Value::String(obs_auth(password, salt, challenge));
    }
    sink.send(Message::Text(
        json!({"op": 1, "d": identify}).to_string().into(),
    ))
    .await?;

    let identified = read_json(&mut stream, events).await?;
    if identified.get("op").and_then(Value::as_i64) != Some(2) {
        bail!("OBS не подтвердил подключение (Identified не получен)");
    }

    {
        let mut guard = status.write().await;
        *guard = ObsStatus {
            connected: true,
            obs_version: None,
            websocket_version,
            error: None,
        };
    }
    info!("OBS: подключён к {}", url);

    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.tick().await; // первый тик срабатывает мгновенно

    // Версию спрашиваем повторно, а не один раз при подключении.
    //
    // Обычный сценарий у актёра: агент сам запускает OBS и подключается через
    // секунду. Сокет к этому моменту уже принимает соединения, но OBS ещё
    // грузится и на GetVersion отвечает не всегда. Первый тик интервала
    // срабатывает мгновенно, дальше — попытки раз в VERSION_RETRY.
    let mut version_probe = tokio::time::interval(VERSION_RETRY);
    let mut version_tries: u8 = 0;
    let mut pending_sweep = tokio::time::interval(PENDING_SWEEP);
    pending_sweep.tick().await; // первый тик мгновенный, чистить пока нечего

    let outcome = loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(msg) = incoming else { break Disconnect::Closed };
                match msg? {
                    Message::Text(text) => dispatch(&text, &mut pending, events),
                    Message::Close(frame) => return Err(close_error(frame)),
                    _ => {}
                }
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break Disconnect::HandleDropped };
                send_command(&mut sink, cmd, &mut pending).await?;
            }
            _ = version_probe.tick(), if version_tries < VERSION_MAX_TRIES => {
                if status.read().await.obs_version.is_some() {
                    // Версия получена — больше не спрашиваем.
                    version_tries = VERSION_MAX_TRIES;
                } else {
                    version_tries += 1;
                    request_obs_version(&mut sink, &mut pending, status.clone()).await?;
                }
            }
            _ = pending_sweep.tick() => {
                // Если OBS завис, но сокет цел, ответы не придут никогда.
                // Вызывающая сторона уже ушла по таймауту; убираем и запись,
                // иначе за долгий эфир очередь растёт без предела.
                let now = Instant::now();
                let before = pending.len();
                pending.retain(|_, p| p.deadline > now);
                let dropped = before - pending.len();
                if dropped > 0 {
                    warn!("OBS не ответил на {dropped} запросов, очередь очищена");
                }
            }
            _ = keepalive.tick() => {
                sink.send(Message::Ping(Vec::new().into())).await?;
            }
        }
    };

    // Разрыв соединения не должен оставлять зависшие запросы.
    for (_, p) in pending.drain() {
        match p.reply {
            Reply::Single(tx) => {
                let _ = tx.send(Err(anyhow!("Соединение с OBS закрыто")));
            }
            Reply::Batch(tx) => {
                let _ = tx.send(Err(anyhow!("Соединение с OBS закрыто")));
            }
        }
    }
    Ok(outcome)
}

async fn send_command<S>(
    sink: &mut S,
    cmd: Command,
    pending: &mut HashMap<String, Pending>,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    // Предохранитель от неограниченного роста: срабатывает только если OBS
    // перестал отвечать, а команды продолжают идти потоком.
    if pending.len() >= PENDING_MAX {
        let now = Instant::now();
        pending.retain(|_, p| p.deadline > now);
        if pending.len() >= PENDING_MAX {
            reject(cmd, "OBS не отвечает: слишком много запросов в очереди");
            return Ok(());
        }
    }

    let id = Uuid::new_v4().to_string();
    let (frame, slot) = match cmd {
        Command::Request {
            request_type,
            request_data,
            reply,
        } => (
            json!({"op": 6, "d": {
                "requestType": request_type,
                "requestId": id,
                "requestData": request_data,
            }}),
            Pending::new(Reply::Single(reply)),
        ),
        Command::Batch { requests, reply } => {
            let items: Vec<Value> = requests
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    json!({
                        "requestType": r.request_type,
                        "requestId": format!("{id}-{i}"),
                        "requestData": r.request_data,
                    })
                })
                .collect();
            (
                json!({"op": 8, "d": {
                    "requestId": id,
                    "haltOnFailure": false,
                    "executionType": 0,
                    "requests": items,
                }}),
                Pending::new(Reply::Batch(reply)),
            )
        }
    };
    pending.insert(id, slot);
    sink.send(Message::Text(frame.to_string().into())).await?;
    Ok(())
}

fn dispatch(text: &str, pending: &mut HashMap<String, Pending>, events: &broadcast::Sender<Value>) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return;
    };
    match v.get("op").and_then(Value::as_i64) {
        // Event
        Some(5) => {
            let _ = events.send(v.get("d").cloned().unwrap_or_else(|| json!({})));
        }
        // RequestResponse
        Some(7) => {
            let Some(id) = v.pointer("/d/requestId").and_then(Value::as_str) else {
                return;
            };
            if let Some(Pending {
                reply: Reply::Single(tx),
                ..
            }) = pending.remove(id)
            {
                let _ = tx.send(parse_response(v.pointer("/d").unwrap_or(&Value::Null)));
            }
        }
        // RequestBatchResponse
        Some(9) => {
            let Some(id) = v.pointer("/d/requestId").and_then(Value::as_str) else {
                return;
            };
            if let Some(Pending {
                reply: Reply::Batch(tx),
                ..
            }) = pending.remove(id)
            {
                let results = v
                    .pointer("/d/results")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let _ = tx.send(Ok(results));
            }
        }
        _ => {}
    }
}

fn parse_response(d: &Value) -> Result<Value> {
    let status = d.pointer("/requestStatus").unwrap_or(&Value::Null);
    if status.get("result").and_then(Value::as_bool) == Some(true) {
        return Ok(d.get("responseData").cloned().unwrap_or_else(|| json!({})));
    }
    let comment = status
        .get("comment")
        .and_then(Value::as_str)
        .unwrap_or("OBS отклонил команду");
    Err(anyhow!("{comment}"))
}

/// Читает следующее не-событие, попутно раздавая события подписчикам.
async fn read_json<S>(stream: &mut S, events: &broadcast::Sender<Value>) -> Result<Value>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let Some(msg) = stream.next().await else {
            bail!("OBS закрыл соединение до завершения рукопожатия");
        };
        match msg? {
            Message::Text(text) => {
                let v: Value = serde_json::from_str(&text)?;
                if v.get("op").and_then(Value::as_i64) == Some(5) {
                    let _ = events.send(v.get("d").cloned().unwrap_or_else(|| json!({})));
                    continue;
                }
                return Ok(v);
            }
            Message::Close(frame) => return Err(close_error(frame)),
            _ => continue,
        }
    }
}

/// Пиковые уровни из события InputVolumeMeters, в dB на источник.
///
/// OBS присылает по каналу тройку множителей [magnitude, peak, inputPeak];
/// берём наибольшее значение среди всех каналов и переводим в dB. Источники
/// без звука OBS присылает с пустым списком уровней — их пропускаем, иначе
/// панель показывала бы им вечную тишину.
pub fn meter_levels(event_data: &Value) -> Vec<(String, f64)> {
    event_data
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|input| {
            let name = input.get("inputName").and_then(Value::as_str)?;
            let channels = input.get("inputLevelsMul").and_then(Value::as_array)?;
            let peak = channels
                .iter()
                .filter_map(Value::as_array)
                .flatten()
                .filter_map(Value::as_f64)
                .fold(f64::NAN, f64::max);
            // Тишина приходит не нулём, а исчезающе малым множителем, и
            // формула даёт значения вроде -190 dB. Прижимаем к тому же полу,
            // что и остальные преобразования, чтобы шкала была одна.
            peak.is_finite()
                .then(|| (name.to_string(), mul_to_db(peak).max(DB_FLOOR)))
        })
        .collect()
}

/// Ставит GetVersion в очередь как обычный запрос и дописывает версию
/// в статус, когда ответ придёт. Не блокирует подключение.
async fn request_obs_version<S>(
    sink: &mut S,
    pending: &mut HashMap<String, Pending>,
    status: Arc<RwLock<ObsStatus>>,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    pending.insert(id.clone(), Pending::new(Reply::Single(tx)));
    sink.send(Message::Text(
        json!({"op": 6, "d": {
            "requestType": "GetVersion",
            "requestId": id,
            "requestData": {},
        }})
        .to_string()
        .into(),
    ))
    .await?;

    tokio::spawn(async move {
        let Ok(Ok(data)) = rx.await else { return };
        let Some(version) = data.get("obsVersion").and_then(Value::as_str) else {
            return;
        };
        let mut guard = status.write().await;
        // Соединение могло уже оборваться, пока ответ шёл — не воскрешаем статус.
        if guard.connected {
            guard.obs_version = Some(version.to_string());
        }
    });
    Ok(())
}

fn close_error(frame: Option<CloseFrame>) -> anyhow::Error {
    let Some(frame) = frame else {
        return anyhow!("OBS закрыл соединение");
    };
    match u16::from(frame.code) {
        4009 => anyhow!(
            "OBS отклонил пароль WebSocket. Запустите AccessibleOBS.exe в режиме актёра — он пересоздаст пароль и настроит OBS."
        ),
        4008 => anyhow!("OBS требует авторизацию, а пароль не был отправлен"),
        code => anyhow!("OBS закрыл соединение (код {code}): {}", frame.reason),
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
    let lower = e.to_ascii_lowercase();
    if lower.contains("econnrefused")
        || lower.contains("connection refused")
        || lower.contains("не удалось подключиться")
    {
        "OBS не отвечает. Проверьте, что OBS Studio запущен и WebSocket-сервер включён.".into()
    } else {
        e.to_string()
    }
}

/// Ниже этого значения громкость считается нулевой. OBS оперирует
/// множителями, и у тишины он даёт не ноль, а исчезающе малое число.
pub const DB_FLOOR: f64 = -100.0;

pub fn db_to_mul(db: f64) -> f64 {
    if db <= DB_FLOOR {
        0.0
    } else {
        10f64.powf(db / 20.0)
    }
}
pub fn mul_to_db(mul: f64) -> f64 {
    if mul <= 0.0 {
        DB_FLOOR
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

    #[test]
    fn event_subscriptions_cover_panel_needs() {
        assert_eq!(EVENT_SUBSCRIPTIONS & SUB_SCENES, SUB_SCENES);
        assert_eq!(EVENT_SUBSCRIPTIONS & SUB_INPUTS, SUB_INPUTS);
        assert_eq!(EVENT_SUBSCRIPTIONS & SUB_OUTPUTS, SUB_OUTPUTS);
        assert_eq!(EVENT_SUBSCRIPTIONS & SUB_SCENE_ITEMS, SUB_SCENE_ITEMS);
    }

    #[test]
    fn successful_response_yields_data() {
        let d = json!({
            "requestId": "x",
            "requestStatus": {"result": true, "code": 100},
            "responseData": {"sceneName": "Main"}
        });
        let out = parse_response(&d).expect("успешный ответ");
        assert_eq!(out["sceneName"], "Main");
    }

    #[test]
    fn failed_response_surfaces_obs_comment() {
        let d = json!({
            "requestId": "x",
            "requestStatus": {"result": false, "code": 600, "comment": "No such scene"}
        });
        let err = parse_response(&d).expect_err("ошибка OBS");
        assert!(err.to_string().contains("No such scene"));
    }

    #[test]
    fn response_data_skips_failed_batch_entries() {
        let ok = json!({"requestStatus": {"result": true}, "responseData": {"inputMuted": false}});
        let failed = json!({"requestStatus": {"result": false, "comment": "gone"}});
        assert_eq!(response_data(&ok).unwrap()["inputMuted"], false);
        assert!(response_data(&failed).is_none());
    }

    #[test]
    fn events_reach_subscribers_during_handshake_reads() {
        let (tx, mut rx) = broadcast::channel(4);
        let event = json!({"eventType": "CurrentProgramSceneChanged"});
        tx.send(event.clone()).unwrap();
        assert_eq!(rx.try_recv().unwrap(), event);
    }

    // --- маршрутизация сообщений OBS ---
    //
    // Через dispatch проходит каждый ответ и каждое событие OBS. Ошибка здесь
    // означает либо зависший запрос, либо мёртвую панель, поэтому разбираем
    // все ветки отдельно.

    fn test_channel() -> broadcast::Sender<Value> {
        broadcast::channel(8).0
    }

    #[test]
    fn dispatch_delivers_response_to_the_waiting_request() {
        let events = test_channel();
        let mut pending = HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        pending.insert("req-1".to_string(), Pending::new(Reply::Single(tx)));

        dispatch(
            &json!({"op": 7, "d": {
                "requestId": "req-1",
                "requestStatus": {"result": true, "code": 100},
                "responseData": {"obsVersion": "32.2.1"},
            }})
            .to_string(),
            &mut pending,
            &events,
        );

        let data = rx.try_recv().expect("ответ дошёл").expect("успех");
        assert_eq!(data["obsVersion"], "32.2.1");
        assert!(pending.is_empty(), "выполненный запрос убран из очереди");
    }

    #[test]
    fn dispatch_passes_obs_refusal_to_the_caller() {
        let events = test_channel();
        let mut pending = HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        pending.insert("req-2".to_string(), Pending::new(Reply::Single(tx)));

        dispatch(
            &json!({"op": 7, "d": {
                "requestId": "req-2",
                "requestStatus": {"result": false, "comment": "Сцена не найдена"},
            }})
            .to_string(),
            &mut pending,
            &events,
        );

        let err = rx.try_recv().expect("ответ дошёл").expect_err("отказ OBS");
        assert!(err.to_string().contains("Сцена не найдена"));
    }

    #[test]
    fn dispatch_broadcasts_events_to_the_panel() {
        let events = test_channel();
        let mut rx = events.subscribe();
        let mut pending = HashMap::new();

        dispatch(
            &json!({"op": 5, "d": {
                "eventType": "InputMuteStateChanged",
                "eventData": {"inputName": "Микрофон", "inputMuted": true},
            }})
            .to_string(),
            &mut pending,
            &events,
        );

        let event = rx.try_recv().expect("событие ушло в панель");
        assert_eq!(event["eventType"], "InputMuteStateChanged");
        assert_eq!(event["eventData"]["inputMuted"], true);
    }

    #[test]
    fn dispatch_returns_batch_results_in_order() {
        let events = test_channel();
        let mut pending = HashMap::new();
        let (tx, mut rx) = oneshot::channel();
        pending.insert("batch-1".to_string(), Pending::new(Reply::Batch(tx)));

        dispatch(
            &json!({"op": 9, "d": {
                "requestId": "batch-1",
                "results": [
                    {"requestStatus": {"result": true}, "responseData": {"inputVolumeDb": -6.0}},
                    {"requestStatus": {"result": true}, "responseData": {"inputMuted": false}},
                ],
            }})
            .to_string(),
            &mut pending,
            &events,
        );

        let results = rx.try_recv().expect("ответ дошёл").expect("успех");
        assert_eq!(results.len(), 2);
        assert_eq!(response_data(&results[0]).unwrap()["inputVolumeDb"], -6.0);
        assert_eq!(response_data(&results[1]).unwrap()["inputMuted"], false);
    }

    #[test]
    fn dispatch_ignores_response_without_a_waiting_request() {
        let events = test_channel();
        let mut pending = HashMap::new();
        // Ответ на запрос, который уже отменили по таймауту, не должен ничего ломать.
        dispatch(
            &json!({"op": 7, "d": {
                "requestId": "давно-забытый",
                "requestStatus": {"result": true},
            }})
            .to_string(),
            &mut pending,
            &events,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn stale_pending_entries_are_swept() {
        // OBS завис, но сокет цел: ответы не придут никогда. Вызывающая
        // сторона уже ушла по таймауту, и запись обязана исчезнуть — иначе
        // за долгий эфир очередь растёт без предела.
        let mut pending = HashMap::new();
        let (tx, _rx) = oneshot::channel();
        let mut entry = Pending::new(Reply::Single(tx));
        entry.deadline = Instant::now() - Duration::from_secs(1);
        pending.insert("протухший".to_string(), entry);

        let (tx2, _rx2) = oneshot::channel();
        pending.insert("свежий".to_string(), Pending::new(Reply::Single(tx2)));

        let now = Instant::now();
        pending.retain(|_, p| p.deadline > now);

        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key("свежий"));
    }

    #[test]
    fn pending_deadline_outlives_caller_timeout() {
        // Запас нужен, чтобы дошедший в последний момент ответ ещё нашёл
        // своего получателя, а не был выброшен уборкой.
        let (tx, _rx) = oneshot::channel();
        let entry = Pending::new(Reply::Single(tx));
        assert!(entry.deadline > Instant::now() + REQUEST_TIMEOUT);
    }

    #[test]
    fn dispatch_survives_garbage_from_the_socket() {
        let events = test_channel();
        let mut pending = HashMap::new();
        // Ни одно из этих сообщений не должно ронять соединение.
        for junk in ["не json", "{}", r#"{"op": 42}"#, r#"{"op": 7}"#] {
            dispatch(junk, &mut pending, &events);
        }
    }

    // --- сообщения при разрыве ---

    fn close_frame(code: u16, reason: &str) -> CloseFrame {
        CloseFrame {
            code: code.into(),
            reason: reason.into(),
        }
    }

    #[test]
    fn wrong_password_close_tells_the_user_what_to_run() {
        // 4009 — единственный код, который пользователь может исправить сам,
        // поэтому в тексте обязана быть конкретная инструкция.
        let msg = close_error(Some(close_frame(4009, "Authentication failed"))).to_string();
        assert!(msg.contains("пароль"), "названа причина: {msg}");
        assert!(msg.contains("AccessibleOBS.exe"), "названо решение: {msg}");
    }

    #[test]
    fn unknown_close_code_keeps_code_and_reason_visible() {
        let msg = close_error(Some(close_frame(1001, "Server stopping"))).to_string();
        assert!(msg.contains("1001"));
        assert!(msg.contains("Server stopping"));
    }

    // --- уровни звука ---

    #[test]
    fn meter_levels_take_loudest_channel() {
        // Стерео: правый канал громче, показать надо его.
        let levels = meter_levels(&json!({
            "inputs": [{
                "inputName": "Микрофон",
                "inputLevelsMul": [[0.05, 0.06, 0.06], [0.4, 0.5, 0.5]],
            }],
        }));
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].0, "Микрофон");
        // 0.5 множителя — примерно -6 dB.
        assert!(
            (levels[0].1 - (-6.02)).abs() < 0.1,
            "получено {} dB",
            levels[0].1
        );
    }

    #[test]
    fn meter_levels_skip_inputs_without_audio() {
        // Источники без звука приходят с пустым списком уровней. Показывать им
        // вечную тишину было бы враньём — их вообще не должно быть в списке.
        let levels = meter_levels(&json!({
            "inputs": [
                {"inputName": "Захват экрана", "inputLevelsMul": []},
                {"inputName": "Микрофон", "inputLevelsMul": [[0.25, 0.25, 0.25]]},
            ],
        }));
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].0, "Микрофон");
    }

    #[test]
    fn meter_levels_report_silence_as_floor() {
        let levels = meter_levels(&json!({
            "inputs": [{"inputName": "Тишина", "inputLevelsMul": [[0.0, 0.0, 0.0]]}],
        }));
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].1, DB_FLOOR);
    }

    #[test]
    fn meter_levels_clamp_near_silence_to_floor() {
        // OBS отдаёт тишину не нулём, а числом вроде 3e-10: без прижатия
        // формула давала бы -190 dB, чего нет ни на одной шкале.
        let levels = meter_levels(&json!({
            "inputs": [{"inputName": "Почти тишина", "inputLevelsMul": [[3e-10, 3e-10, 3e-10]]}],
        }));
        assert_eq!(levels[0].1, DB_FLOOR);
    }

    #[test]
    fn meter_levels_tolerate_missing_fields() {
        assert!(meter_levels(&Value::Null).is_empty());
        assert!(meter_levels(&json!({"inputs": []})).is_empty());
        assert!(meter_levels(&json!({"inputs": [{"inputName": "Без уровней"}]})).is_empty());
    }

    #[test]
    fn close_without_frame_still_reports_disconnect() {
        assert!(close_error(None).to_string().contains("закрыл соединение"));
    }
}
