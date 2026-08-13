pub mod donationalerts;
pub mod obs;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::timeout;

pub const APP_NAME: &str = "Remote Stream Control";
pub const OVERLAY_SCENE: &str = "RSC_OVERLAYS";
pub const DA_INPUT: &str = "RSC_DonationAlerts";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default)]
    pub obs_path: String,
    #[serde(default = "default_obs_host")]
    pub obs_websocket_host: String,
    #[serde(default = "default_obs_port")]
    pub obs_websocket_port: u16,
    #[serde(default)]
    pub obs_websocket_password: String,
    #[serde(default = "default_listen_mode")]
    pub listen_mode: String,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    #[serde(default = "default_true")]
    pub auto_start_obs: bool,
    #[serde(default = "default_true")]
    pub enable_tailscale_unattended_after_login: bool,
    #[serde(default)]
    pub twitch: TwitchConfig,
    #[serde(default)]
    pub donationalerts: DonationAlertsConfig,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerConfig {
    #[serde(default = "default_friend_name")]
    pub friend_machine_name: String,
    #[serde(default)]
    pub friend_tailscale_ip_fallback: String,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    #[serde(default = "default_true")]
    pub auto_open_browser: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TwitchConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_twitch_scopes")]
    pub scopes: Vec<String>,
}
impl Default for TwitchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            client_id: String::new(),
            scopes: default_twitch_scopes(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DonationAlertsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default = "default_overlay_scene")]
    pub overlay_scene_name: String,
    #[serde(default = "default_da_input")]
    pub input_name: String,
    #[serde(default = "default_true")]
    pub enforce_overlays: bool,
    #[serde(default = "default_browser_size")]
    pub browser_width: String,
    #[serde(default = "default_browser_size")]
    pub browser_height: String,
    #[serde(default = "default_true")]
    pub reroute_audio: bool,
    #[serde(default = "default_da_volume")]
    pub initial_volume_db: f64,
    #[serde(default = "default_monitoring")]
    pub monitoring: String,
    #[serde(default = "default_true")]
    pub oauth_enabled: bool,
    #[serde(default = "default_da_scopes")]
    pub oauth_scopes: Vec<String>,
    #[serde(default)]
    pub announce_new_donations_to_nvda: bool,
}
impl Default for DonationAlertsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            overlay_scene_name: default_overlay_scene(),
            input_name: default_da_input(),
            enforce_overlays: true,
            browser_width: default_browser_size(),
            browser_height: default_browser_size(),
            reroute_audio: true,
            initial_volume_db: -8.0,
            monitoring: default_monitoring(),
            oauth_enabled: true,
            oauth_scopes: default_da_scopes(),
            announce_new_donations_to_nvda: false,
        }
    }
}
fn default_obs_host() -> String {
    "127.0.0.1".into()
}
fn default_obs_port() -> u16 {
    4455
}
fn default_web_port() -> u16 {
    8787
}
fn default_true() -> bool {
    true
}
fn default_listen_mode() -> String {
    "tailscale_only".into()
}
fn default_friend_name() -> String {
    "friend-pc".into()
}
fn default_twitch_scopes() -> Vec<String> {
    vec!["channel:manage:broadcast".into()]
}
fn default_da_scopes() -> Vec<String> {
    vec![
        "oauth-user-show".into(),
        "oauth-donation-subscribe".into(),
        "oauth-donation-index".into(),
    ]
}
fn default_overlay_scene() -> String {
    OVERLAY_SCENE.into()
}
fn default_da_input() -> String {
    DA_INPUT.into()
}
fn default_browser_size() -> String {
    "obs_canvas".into()
}
fn default_da_volume() -> f64 {
    -8.0
}
fn default_monitoring() -> String {
    "off".into()
}

/// Корень установки: папка, содержащая `bin`, `config`, `web` и `logs`.
///
/// Берётся от пути к самому exe, а не от текущей директории, иначе запуск
/// `bin\host-agent.exe` двойным кликом искал бы config рядом с exe и не нашёл.
/// При `cargo run` exe лежит в `target\debug`, и мы честно падаем в cwd.
pub fn app_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
            && dir
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("bin"))
            && let Some(root) = dir.parent()
        {
            return root.to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    })
    .clone()
}
pub fn config_dir() -> PathBuf {
    app_root().join("config")
}
pub fn logs_dir() -> PathBuf {
    app_root().join("logs")
}
pub fn web_dir() -> PathBuf {
    app_root().join("web")
}
pub fn bin_dir() -> PathBuf {
    app_root().join("bin")
}
pub fn secrets_dir() -> PathBuf {
    config_dir().join("secrets")
}

pub fn ensure_dirs() -> Result<()> {
    for p in [
        config_dir(),
        logs_dir(),
        web_dir(),
        bin_dir(),
        secrets_dir(),
    ] {
        fs::create_dir_all(p)?;
    }
    Ok(())
}

pub fn load_host_config() -> Result<HostConfig> {
    ensure_dirs()?;
    let path = config_dir().join("host.json");
    if !path.exists() {
        let cfg = HostConfig::default();
        save_json(&path, &cfg)?;
        return Ok(cfg);
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&strip_bom(&text))?)
}
impl Default for HostConfig {
    fn default() -> Self {
        Self {
            obs_path: String::new(),
            obs_websocket_host: default_obs_host(),
            obs_websocket_port: 4455,
            obs_websocket_password: String::new(),
            listen_mode: default_listen_mode(),
            web_port: 8787,
            auto_start_obs: true,
            enable_tailscale_unattended_after_login: true,
            twitch: TwitchConfig::default(),
            donationalerts: DonationAlertsConfig::default(),
        }
    }
}
pub fn load_controller_config() -> Result<ControllerConfig> {
    ensure_dirs()?;
    let path = config_dir().join("controller.json");
    if !path.exists() {
        let cfg = ControllerConfig {
            friend_machine_name: default_friend_name(),
            friend_tailscale_ip_fallback: String::new(),
            web_port: 8787,
            auto_open_browser: true,
        };
        save_json(&path, &cfg)?;
        return Ok(cfg);
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&strip_bom(&text))?)
}
pub fn save_json<T: Serialize>(path: &Path, val: &T) -> Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    fs::write(path, serde_json::to_string_pretty(val)?)?;
    Ok(())
}
fn strip_bom(s: &str) -> String {
    s.trim_start_matches('\u{feff}').to_string()
}

pub fn random_secret_b64(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// Режем по символам, а не по байтам: срез `&s[..4]` паникует, если четвёртый
/// байт попадает в середину UTF-8 последовательности.
pub fn redact(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        return "***".into();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

#[cfg(windows)]
pub mod protected {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut output = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: null_mut(),
            };
            let ok = CryptProtectData(
                &input,
                null(),
                null(),
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            );
            if ok == 0 {
                return Err(anyhow!("CryptProtectData failed"));
            }
            let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData as *mut _);
            Ok(out)
        }
    }
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        unsafe {
            let input = CRYPT_INTEGER_BLOB {
                cbData: data.len() as u32,
                pbData: data.as_ptr() as *mut u8,
            };
            let mut output = CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: null_mut(),
            };
            let ok = CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            );
            if ok == 0 {
                return Err(anyhow!("CryptUnprotectData failed"));
            }
            let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData as *mut _);
            Ok(out)
        }
    }
}
#[cfg(not(windows))]
pub mod protected {
    use super::*;
    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

pub fn secret_path(name: &str) -> PathBuf {
    secrets_dir().join(format!("{}.dpapi", name))
}
pub fn save_secret(name: &str, value: &str) -> Result<()> {
    ensure_dirs()?;
    let enc = protected::protect(value.as_bytes())?;
    fs::write(secret_path(name), enc)?;
    Ok(())
}
pub fn load_secret(name: &str) -> Result<Option<String>> {
    let p = secret_path(name);
    if !p.exists() {
        return Ok(None);
    };
    let data = fs::read(p)?;
    let dec = protected::unprotect(&data)?;
    Ok(Some(String::from_utf8(dec)?))
}
pub fn delete_secret(name: &str) -> Result<()> {
    let p = secret_path(name);
    if p.exists() {
        fs::remove_file(p)?
    };
    Ok(())
}

pub fn find_exe(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

#[cfg(windows)]
pub fn default_obs_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files\obs-studio\bin\64bit\obs64.exe"),
        PathBuf::from(r"C:\Program Files (x86)\obs-studio\bin\64bit\obs64.exe"),
    ]
}
#[cfg(not(windows))]
pub fn default_obs_paths() -> Vec<PathBuf> {
    vec![]
}

pub fn find_obs(configured: &str) -> Option<PathBuf> {
    if !configured.is_empty() && Path::new(configured).exists() {
        return Some(PathBuf::from(configured));
    }
    for p in default_obs_paths() {
        if p.exists() {
            return Some(p);
        }
    }
    find_exe("obs64.exe").or_else(|| find_exe("obs"))
}

pub fn process_running(name: &str) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .contains(&name.to_ascii_lowercase())
        })
        .unwrap_or(false)
}

pub fn tailscale_exe() -> Option<PathBuf> {
    find_exe("tailscale.exe").or_else(|| {
        let p = PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
        p.exists().then_some(p)
    })
}

/// IPv4-адрес машины в tailnet. `None`, если Tailscale не установлен или не поднят.
pub fn tailscale_ip() -> Option<IpAddr> {
    let exe = tailscale_exe()?;
    let out = Command::new(exe).args(["ip", "-4"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

pub fn tailscale_running() -> bool {
    let Some(exe) = tailscale_exe() else {
        return false;
    };
    Command::new(exe)
        .args(["status", "--json"])
        .output()
        .map(|o| {
            let txt = String::from_utf8_lossy(&o.stdout);
            txt.contains("\"BackendState\":\"Running\"")
                || txt.contains("\"BackendState\": \"Running\"")
        })
        .unwrap_or(false)
}

pub fn obs_is_running() -> bool {
    process_running("obs64.exe") || process_running("obs.exe")
}

static OBS_CRASHED_LAST_RUN: AtomicBool = AtomicBool::new(false);

/// Завершился ли прошлый сеанс OBS аварийно.
pub fn obs_crashed_last_run() -> bool {
    OBS_CRASHED_LAST_RUN.load(Ordering::Relaxed)
}

/// Убирает признак аварийного завершения OBS перед запуском.
///
/// OBS создаёт `.sentinel` при старте и удаляет при штатном выходе. Если файл
/// уцелел, следующий запуск показывает модальное окно безопасного режима и ждёт
/// человека у клавиатуры. У актёра человека нет: одно падение OBS означало бы
/// потерю управления до тех пор, пока кто-то физически не нажмёт кнопку.
///
/// Плата за это осознанная: при плагине, роняющем OBS на старте, вместо
/// предложения безопасного режима получится цикл падений. Поэтому факт падения
/// не проглатываем, а показываем владельцу в панели.
#[cfg(windows)]
pub fn clear_obs_crash_sentinel() -> bool {
    let Ok(appdata) = std::env::var("APPDATA") else {
        return false;
    };
    // `.sentinel` — это папка: OBS кладёт туда файл `run_<uuid>` на каждый
    // запуск и удаляет его при штатном выходе. Уцелевшие файлы означают
    // аварийно завершённые сеансы.
    let dir = PathBuf::from(appdata).join("obs-studio").join(".sentinel");
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    let mut crashed = false;
    for entry in entries.flatten() {
        let is_run_marker = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("run_"));
        if is_run_marker && fs::remove_file(entry.path()).is_ok() {
            crashed = true;
        }
    }
    if crashed {
        OBS_CRASHED_LAST_RUN.store(true, Ordering::Relaxed);
    }
    crashed
}
#[cfg(not(windows))]
pub fn clear_obs_crash_sentinel() -> bool {
    false
}

/// Запускает OBS, если он ещё не запущен. Свёрнутым в трей, чтобы у актёра
/// не открывалось окно на весь экран при каждом входе в Windows.
pub fn start_obs_if_needed(configured_path: &str) -> Result<bool> {
    if obs_is_running() {
        return Ok(false);
    }
    if clear_obs_crash_sentinel() {
        tracing::warn!("Прошлый сеанс OBS завершился аварийно; окно безопасного режима подавлено");
    }
    let obs = find_obs(configured_path).ok_or_else(|| {
        anyhow!("OBS Studio не найден. Установите OBS с https://obsproject.com/ и запустите START_FRIEND.bat.")
    })?;
    let mut cmd = Command::new(&obs);
    if let Some(parent) = obs.parent() {
        cmd.current_dir(parent);
    }
    cmd.arg("--minimize-to-tray")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("не удалось запустить {}", obs.display()))?;
    Ok(true)
}

#[cfg(windows)]
pub fn startup_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
}
#[cfg(not(windows))]
pub fn startup_dir() -> Option<PathBuf> {
    None
}

pub fn autostart_shortcut() -> Option<PathBuf> {
    Some(startup_dir()?.join("Remote Stream Control.lnk"))
}

pub fn autostart_registered() -> bool {
    autostart_shortcut().is_some_and(|p| p.exists())
}

/// Прописывает host-agent в автозагрузку текущего пользователя.
///
/// Ярлык, а не ключ реестра Run: ярлык виден пользователю в папке
/// «Автозагрузка», его легко удалить вручную, и он не требует прав администратора.
/// Создаём через WScript.Shell — единственный способ собрать .lnk без COM-крейта.
#[cfg(windows)]
pub fn register_autostart() -> Result<PathBuf> {
    let shortcut = autostart_shortcut().context("не удалось определить папку автозагрузки")?;
    let target = bin_dir().join("host-agent.exe");
    if !target.exists() {
        bail!("{} не найден", target.display());
    }
    fs::create_dir_all(shortcut.parent().unwrap())?;

    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.Description = 'Remote Stream Control host agent'; \
         $s.Save()",
        ps_quote(&shortcut.to_string_lossy()),
        ps_quote(&target.to_string_lossy()),
        ps_quote(&app_root().to_string_lossy()),
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("не удалось запустить powershell для создания ярлыка")?;
    if !shortcut.exists() {
        bail!(
            "ярлык автозагрузки не создан: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(shortcut)
}
#[cfg(not(windows))]
pub fn register_autostart() -> Result<PathBuf> {
    bail!("автозапуск поддерживается только на Windows")
}

pub fn unregister_autostart() -> Result<()> {
    if let Some(p) = autostart_shortcut()
        && p.exists()
    {
        fs::remove_file(p)?;
    }
    Ok(())
}

/// Экранирование для одинарных кавычек PowerShell: удвоение апострофа.
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// Путь к настройкам obs-websocket.
///
/// Начиная с OBS 28 плагин obs-websocket встроен и хранит конфигурацию здесь,
/// а НЕ в `global.ini`. Прежняя версия писала секцию `[OBSWebSocket]` в
/// global.ini, которую современный OBS просто игнорирует, поэтому пароль
/// никогда не применялся и агент не мог авторизоваться.
#[cfg(windows)]
pub fn obs_websocket_config_path() -> Result<PathBuf> {
    let appdata = std::env::var("APPDATA").context("переменная APPDATA не задана")?;
    Ok(PathBuf::from(appdata)
        .join("obs-studio")
        .join("plugin_config")
        .join("obs-websocket")
        .join("config.json"))
}

#[cfg(windows)]
fn read_obs_websocket_config() -> Value {
    let Ok(path) = obs_websocket_config_path() else {
        return json!({});
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return json!({});
    };
    match serde_json::from_str::<Value>(&strip_bom(&text)) {
        Ok(v) if v.is_object() => v,
        _ => json!({}),
    }
}

/// Вливает нужные ключи в существующий конфиг, не трогая чужие.
/// Возвращает результат и признак того, что что-то поменялось.
fn merge_config(mut current: Value, desired: &Value) -> (Value, bool) {
    if !current.is_object() {
        current = json!({});
    }
    let obj = current.as_object_mut().expect("объект гарантирован выше");
    let mut changed = false;
    for (key, value) in desired.as_object().expect("desired всегда объект") {
        if obj.get(key) != Some(value) {
            obj.insert(key.clone(), value.clone());
            changed = true;
        }
    }
    (current, changed)
}

/// Настройки, которые нам нужны от obs-websocket.
fn desired_obs_websocket_config(password: &str, port: u16) -> Value {
    json!({
        "server_enabled": true,
        "auth_required": true,
        "server_port": port,
        "server_password": password,
        // Без этого ключа OBS показывает мастер первого запуска плагина.
        "first_load": false,
    })
}

/// Пароль, который OBS уже использует, если он там задан.
///
/// Подхватываем его вместо генерации нового: к WebSocket мог быть подключён
/// Streamer.bot, Touch Portal или чужой пульт, и смена пароля их сломала бы.
#[cfg(windows)]
pub fn existing_obs_websocket_password() -> Option<String> {
    read_obs_websocket_config()
        .get("server_password")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
#[cfg(not(windows))]
pub fn existing_obs_websocket_password() -> Option<String> {
    None
}

/// Совпадают ли текущие настройки OBS с нужными.
///
/// Проверять отдельно важно: OBS перезаписывает config.json при выходе, поэтому
/// менять файл имеет смысл только когда OBS закрыт.
#[cfg(windows)]
pub fn obs_websocket_config_matches(password: &str, port: u16) -> bool {
    let current = read_obs_websocket_config();
    desired_obs_websocket_config(password, port)
        .as_object()
        .expect("desired всегда объект")
        .iter()
        .all(|(k, v)| current.get(k) == Some(v))
}
#[cfg(not(windows))]
pub fn obs_websocket_config_matches(_password: &str, _port: u16) -> bool {
    true
}

/// Включает WebSocket-сервер OBS с нашим паролем. Возвращает `true`, если файл
/// действительно менялся.
///
/// Остальные ключи (например `alerts_enabled`) сохраняем как есть — это
/// пользовательские настройки, и затирать их незачем.
#[cfg(windows)]
pub fn ensure_obs_websocket_config(password: &str, port: u16) -> Result<bool> {
    let path = obs_websocket_config_path()?;
    fs::create_dir_all(path.parent().context("нет родительской папки")?)?;

    let desired = desired_obs_websocket_config(password, port);
    let (cfg, changed) = merge_config(read_obs_websocket_config(), &desired);
    if changed {
        fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
    }
    Ok(changed)
}
#[cfg(not(windows))]
pub fn ensure_obs_websocket_config(_password: &str, _port: u16) -> Result<bool> {
    Ok(false)
}

pub async fn tcp_ready(host: &str, port: u16, ms: u64) -> bool {
    timeout(
        Duration::from_millis(ms),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl ApiError {
    pub fn new(m: impl Into<String>) -> Self {
        Self {
            message: m.into(),
            detail: None,
        }
    }
    pub fn detail(m: impl Into<String>, d: impl Into<String>) -> Self {
        Self {
            message: m.into(),
            detail: Some(d.into()),
        }
    }
}

pub fn ok_json<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or_else(|_| json!({"ok":true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_config_defaults_match_expected_ports_and_reserved_names() {
        let cfg = HostConfig::default();
        assert_eq!(cfg.obs_websocket_host, "127.0.0.1");
        assert_eq!(cfg.obs_websocket_port, 4455);
        assert_eq!(cfg.web_port, 8787);
        assert_eq!(cfg.listen_mode, "tailscale_only");
        assert_eq!(cfg.donationalerts.overlay_scene_name, OVERLAY_SCENE);
        assert_eq!(cfg.donationalerts.input_name, DA_INPUT);
        assert!(cfg.donationalerts.reroute_audio);
    }

    #[test]
    fn controller_config_defaults_use_magicdns_first() {
        let cfg = ControllerConfig {
            friend_machine_name: default_friend_name(),
            friend_tailscale_ip_fallback: String::new(),
            web_port: default_web_port(),
            auto_open_browser: true,
        };
        assert_eq!(cfg.friend_machine_name, "friend-pc");
        assert_eq!(cfg.web_port, 8787);
    }

    #[test]
    fn websocket_config_enables_server_with_our_password() {
        let desired = desired_obs_websocket_config("s3cret", 4455);
        assert_eq!(desired["server_enabled"], true);
        assert_eq!(desired["auth_required"], true);
        assert_eq!(desired["server_port"], 4455);
        assert_eq!(desired["server_password"], "s3cret");
        // Без first_load=false OBS покажет мастер первого запуска плагина.
        assert_eq!(desired["first_load"], false);
    }

    #[test]
    fn merge_keeps_unrelated_user_settings() {
        // Реальный конфиг OBS 32 с выключенным сервером и чужим паролем.
        let current = json!({
            "alerts_enabled": true,
            "auth_required": true,
            "first_load": false,
            "server_enabled": false,
            "server_password": "их-пароль",
            "server_port": 4455
        });
        let (merged, changed) = merge_config(current, &desired_obs_websocket_config("наш", 4455));
        assert!(changed);
        assert_eq!(merged["server_enabled"], true);
        assert_eq!(merged["server_password"], "наш");
        // Пользовательскую настройку не тронули.
        assert_eq!(merged["alerts_enabled"], true);
    }

    #[test]
    fn merge_reports_no_change_when_already_correct() {
        let desired = desired_obs_websocket_config("pw", 4455);
        let (_, changed) = merge_config(desired.clone(), &desired);
        assert!(!changed);
    }

    #[test]
    fn merge_recovers_from_corrupted_config() {
        let (merged, changed) = merge_config(
            json!("не объект"),
            &desired_obs_websocket_config("pw", 4455),
        );
        assert!(changed);
        assert_eq!(merged["server_password"], "pw");
    }

    #[test]
    fn random_secret_uses_urlsafe_base64_without_padding() {
        let secret = random_secret_b64(24);
        assert!(secret.len() >= 32);
        assert!(!secret.contains('='));
        assert!(
            secret
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn redact_does_not_leak_middle_of_long_secret() {
        assert_eq!(redact("short"), "***");
        assert_eq!(redact("abcdefghijklmnop"), "abcd...mnop");
    }
}
