pub mod donationalerts;
pub mod health;
pub mod obs;
pub mod preflight;
pub mod roles;
pub mod twitch_chat;

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

pub const APP_NAME: &str = "Accessible OBS";
pub const OVERLAY_SCENE: &str = "RSC_OVERLAYS";
pub const DA_INPUT: &str = "RSC_DonationAlerts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    Remote,
    Local,
}

fn default_runtime_mode() -> RuntimeMode {
    RuntimeMode::Remote
}

/// Кому предназначен интерфейс.
///
/// Разделение нужно потому, что «доступно» и «удобно глазами» — разные вещи,
/// а не градации одного. Проектор OBS незрячему бесполезен в принципе: окно
/// проектора это поверхность отрисовки, у неё нет дерева доступности, и
/// экранному диктору там нечего читать. Наоборот, непрерывное зачитывание
/// чата зрячему только мешает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterfaceMode {
    /// Для незрячего: всё важное объявляется вслух, визуальные штуки скрыты.
    Accessible,
    /// Для зрячего: доступен проектор и прочее, что смотрят глазами.
    Standard,
}

/// По умолчанию доступный: проект существует ради него, и лучше показать
/// зрячему лишнюю настройку, чем незрячему — бесполезную кнопку.
fn default_interface_mode() -> InterfaceMode {
    InterfaceMode::Accessible
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default = "default_runtime_mode")]
    pub runtime_mode: RuntimeMode,
    #[serde(default = "default_interface_mode")]
    pub interface_mode: InterfaceMode,
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
/// По умолчанию донат слышат и зрители, и сам владелец.
///
/// Прежде здесь стояло «off», то есть владелец не слышал собственных донатов.
/// Для зрячего это мелочь — он видит алерт на экране. Для незрячего звук
/// единственный способ узнать о донате, а программа написана прежде всего
/// для него.
fn default_monitoring() -> String {
    "both".into()
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
        {
            if dir.join("bin").exists() && dir.join("web").exists() {
                return dir.to_path_buf();
            }
            if dir
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("bin"))
                && let Some(root) = dir.parent()
            {
                return root.to_path_buf();
            }
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
            runtime_mode: default_runtime_mode(),
            interface_mode: default_interface_mode(),
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
/// Роли источников лежат отдельным файлом, а не в host.json и не в хранилище
/// секретов: это не секрет и не настройка соединения, а выбор владельца,
/// который полезно уметь посмотреть и поправить руками.
pub fn roles_path() -> PathBuf {
    config_dir().join("roles.json")
}

pub fn load_source_roles() -> roles::SourceRoles {
    fs::read_to_string(roles_path())
        .map(|raw| roles::SourceRoles::from_json(&strip_bom(&raw)))
        .unwrap_or_default()
}

pub fn save_source_roles(assigned: &roles::SourceRoles) -> Result<()> {
    ensure_dirs()?;
    fs::write(roles_path(), assigned.to_json())?;
    Ok(())
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

/// Разбирает `tailscale status --json`.
///
/// Раньше состояние искали подстрокой в тексте вывода, причём двумя вариантами
/// сразу — с пробелом после двоеточия и без. Форматирование JSON не является
/// контрактом: смена отступов или порядка полей в Tailscale молча сломала бы
/// проверку, и агент решил бы, что сети нет.
fn parse_backend_running(json: &str) -> bool {
    #[derive(Deserialize)]
    struct Status {
        #[serde(rename = "BackendState")]
        backend_state: Option<String>,
    }
    serde_json::from_str::<Status>(json)
        .ok()
        .and_then(|s| s.backend_state)
        .is_some_and(|state| state == "Running")
}

pub fn tailscale_running() -> bool {
    let Some(exe) = tailscale_exe() else {
        return false;
    };
    Command::new(exe)
        .args(["status", "--json"])
        .output()
        .map(|o| parse_backend_running(&String::from_utf8_lossy(&o.stdout)))
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
        anyhow!("OBS Studio не найден. Установите OBS с https://obsproject.com/ или запустите AccessibleOBS.exe в режиме актёра.")
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
    Some(startup_dir()?.join("Accessible OBS.lnk"))
}

/// Имена ярлыков автозагрузки от прежних версий программы.
///
/// Программа называлась иначе, и ярлык в «Автозагрузке» носил то имя. Само по
/// себе переименование его не трогает: агент искал бы ярлык с новым именем, не
/// находил, создавал свой — и при входе в Windows поднимались бы два агента.
/// Второму не достался бы порт, он бы молча лёг, а человек получил бы
/// работающую программу, которая при этом пишет в лог ошибку занятого адреса.
const LEGACY_AUTOSTART_SHORTCUTS: &[&str] = &["Remote Stream Control.lnk"];

fn legacy_autostart_shortcuts() -> Vec<PathBuf> {
    let Some(dir) = startup_dir() else {
        return Vec::new();
    };
    LEGACY_AUTOSTART_SHORTCUTS
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p.exists())
        .collect()
}

/// Убирает ярлыки автозагрузки от прежних имён программы.
fn remove_legacy_autostart() {
    for old in legacy_autostart_shortcuts() {
        match fs::remove_file(&old) {
            Ok(()) => tracing::info!(
                "Убран ярлык автозапуска от прежней версии: {}",
                old.display()
            ),
            Err(e) => tracing::warn!("Не удалось убрать {}: {e}", old.display()),
        }
    }
}

/// Настроен ли автозапуск. Ярлык от прежнего имени тоже считается: он
/// действительно запускает агент, и врать человеку, что автозапуска нет,
/// нельзя.
pub fn autostart_registered() -> bool {
    autostart_shortcut().is_some_and(|p| p.exists()) || !legacy_autostart_shortcuts().is_empty()
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
         $s.Description = 'Accessible OBS host agent'; \
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
    // Только после того, как новый ярлык точно создан: иначе при отказе
    // человек остался бы вовсе без автозапуска.
    remove_legacy_autostart();
    Ok(shortcut)
}
#[cfg(not(windows))]
pub fn register_autostart() -> Result<PathBuf> {
    bail!("автозапуск поддерживается только на Windows")
}

/// Убирает автозапуск — и нынешний ярлык, и оставшиеся от прежних имён.
///
/// Иначе «убрать автозапуск» на начальной странице отвечало бы успехом, а агент
/// продолжал бы подниматься при входе в Windows со старого ярлыка.
pub fn unregister_autostart() -> Result<()> {
    if let Some(p) = autostart_shortcut()
        && p.exists()
    {
        fs::remove_file(p)?;
    }
    remove_legacy_autostart();
    Ok(())
}

/// Включает запись логов в файл и возвращает страж.
///
/// Пока страж жив, фоновый поток дописывает файл; уронив его, потеряем
/// записи. Общая для обоих бинарников: различие было только в имени файла,
/// а копия кода жила своей жизнью.
/// Сколько суточных файлов лога хранить.
///
/// Недели хватает, чтобы разобрать позавчерашний эфир, и при этом папка не
/// растёт бесконечно.
const LOG_FILES_KEPT: usize = 7;

/// Сколько байт с конца читать, когда нужен хвост лога.
///
/// Двухсот строк для разбора достаточно, а 256 КБ заведомо больше, чем они
/// занимают.
const LOG_TAIL_BYTES: u64 = 256 * 1024;

#[must_use = "пока страж жив, пишутся логи; если его уронить, записи пропадут"]
pub fn init_file_logging(file_name: &str) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    // Ротация по суткам с ограничением числа файлов.
    //
    // Прежде файл был один и рос без предела. Агент прописан в автозагрузке и
    // работает месяцами; за это время лог дорастал до сотен мегабайт, а
    // диагностика читала его целиком, чтобы показать последние двести строк.
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(file_name)
        .max_log_files(LOG_FILES_KEPT)
        .build(logs_dir())
        .context("не удалось создать файл лога")?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .ok();
    Ok(guard)
}

/// Запись о стороннем установщике из манифеста.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallerEntry {
    pub file: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// Зафиксированные версии стороннего ПО.
#[derive(Debug, Clone, Deserialize)]
pub struct InstallerManifest {
    pub tailscale: InstallerEntry,
    pub obs: InstallerEntry,
}

pub fn installers_dir() -> PathBuf {
    app_root().join("third_party").join("installers")
}

/// Читает манифест зафиксированных установщиков.
///
/// Один и тот же файл читают и сборщик релиза, и первичная настройка. Прежде
/// он был только у сборщика: тот сверял контрольные суммы, а настройка при
/// отсутствии готового файла качала «последнюю» версию по неизменяемой ссылке
/// и запускала её без всякой проверки. Получались два разных уровня доверия к
/// одному и тому же действию — установке чужой программы на машину актёра.
pub fn load_installer_manifest() -> Result<InstallerManifest> {
    let path = app_root().join("third_party").join("installers.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("манифест установщиков не найден: {}", path.display()))?;
    Ok(serde_json::from_str(&strip_bom(&text))?)
}

/// Контрольная сумма файла.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Убеждается, что файл — именно тот, что записан в манифесте.
///
/// Несовпадение — отказ, а не предупреждение: дальше файл запускается как
/// программа с правами пользователя.
pub fn verify_installer(path: &Path, entry: &InstallerEntry) -> Result<()> {
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if expected.is_empty() {
        bail!(
            "в манифесте нет контрольной суммы для {}; \
             запускать непроверенный установщик нельзя",
            entry.file
        );
    }
    let actual = sha256_file(path)?.to_ascii_lowercase();
    if actual != expected {
        bail!(
            "контрольная сумма {} не совпала.\nОжидалась: {expected}\nПолучена:  {actual}\n\
             Файл повреждён или подменён, запускать его нельзя.",
            path.display()
        );
    }
    Ok(())
}

/// Последние строки самого свежего файла лога.
///
/// Читаем хвост через смещение, а не файл целиком: диагностику вызывают из
/// обработчика запроса, а рабочих потоков у агента всего два, и втягивать в
/// память сотни мегабайт ради двухсот строк нельзя.
///
/// Начало прочитанного куска может попасть в середину строки и разрезать
/// многобайтный символ, поэтому декодируем с потерями и первую строку
/// отбрасываем.
pub fn read_log_tail(file_name: &str, max_lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};

    let Some(path) = newest_log_file(file_name) else {
        return "лог ещё не создан".to_string();
    };
    let Ok(mut file) = fs::File::open(&path) else {
        return format!("лог недоступен: {}", path.display());
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = size.saturating_sub(LOG_TAIL_BYTES);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return "лог не удалось прочитать".to_string();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return "лог не удалось прочитать".to_string();
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Самый свежий файл лога с указанным именем.
///
/// При суточной ротации к имени добавляется дата, поэтому точного совпадения
/// имени недостаточно.
fn newest_log_file(file_name: &str) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(logs_dir()).ok()?.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(file_name))
        {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(best, _)| modified > *best) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Пришёл ли запрос со своей же петлевой страницы.
///
/// Чистая функция на строках, потому что нужна двоим: и панели, и начальной
/// странице. Держать две копии такой проверки — значит однажды исправить одну
/// и забыть про другую.
///
/// Заголовка Origin браузер при простых запросах не шлёт, поэтому запасной
/// признак — Host: обращение по чужому имени, ведущему на 127.0.0.1, выдаёт
/// себя именно им.
pub fn loopback_request_ok(origin: Option<&str>, host: Option<&str>) -> bool {
    let host_allowed = |value: &str| {
        let host = value.rsplit_once(':').map_or(value, |(h, _)| h);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    };

    if let Some(origin) = origin {
        return match origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
        {
            Some(rest) => host_allowed(rest),
            // «null» и прочее нестандартное происхождение доверия не заслуживают.
            None => false,
        };
    }

    // Ни Origin, ни Host — запрос пришёл не от браузера со страницы.
    host.is_some_and(host_allowed)
}

/// Экранирование для одинарных кавычек PowerShell: удвоение апострофа.
pub fn ps_quote(s: &str) -> String {
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
        assert_eq!(cfg.runtime_mode, RuntimeMode::Remote);
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
    fn tailscale_state_is_parsed_not_matched_as_text() {
        // Форматирование JSON не контракт: пробелы и порядок полей могут
        // измениться, а проверка подстрокой молча решила бы, что сети нет.
        assert!(parse_backend_running(r#"{"BackendState":"Running"}"#));
        assert!(parse_backend_running(
            "{\n  \"Version\": \"1.0\",\n  \"BackendState\" : \"Running\"\n}"
        ));
        assert!(!parse_backend_running(r#"{"BackendState":"Stopped"}"#));
        assert!(!parse_backend_running(r#"{"BackendState":"NeedsLogin"}"#));
    }

    #[test]
    fn tailscale_garbage_output_means_not_running() {
        assert!(!parse_backend_running(""));
        assert!(!parse_backend_running("tailscale: command failed"));
        assert!(!parse_backend_running("{}"));
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
    fn old_host_config_without_runtime_mode_stays_remote() {
        let cfg: HostConfig = serde_json::from_value(json!({
            "web_port": 8787,
            "listen_mode": "tailscale_only"
        }))
        .unwrap();
        assert_eq!(cfg.runtime_mode, RuntimeMode::Remote);
        assert_eq!(cfg.listen_mode, "tailscale_only");
    }

    #[test]
    fn host_config_accepts_local_runtime_mode() {
        let cfg: HostConfig = serde_json::from_value(json!({
            "runtime_mode": "local",
            "listen_mode": "tailscale_only"
        }))
        .unwrap();
        assert_eq!(cfg.runtime_mode, RuntimeMode::Local);
        assert_eq!(cfg.listen_mode, "tailscale_only");
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
