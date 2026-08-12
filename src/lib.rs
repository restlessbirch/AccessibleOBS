pub mod obs;

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
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

pub fn app_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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

pub fn redact(s: &str) -> String {
    if s.len() <= 8 {
        "***".into()
    } else {
        format!("{}...{}", &s[..4], &s[s.len() - 4..])
    }
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
pub fn run_output(program: &str, args: &[&str], timeout_ms: u64) -> Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let out = cmd
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    let s =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let _ = timeout_ms;
    Ok(s)
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

pub fn ensure_obs_websocket_config(password: &str, port: u16) -> Result<()> {
    #[cfg(windows)]
    {
        let appdata = std::env::var("APPDATA").context("APPDATA not set")?;
        let path = PathBuf::from(appdata).join("obs-studio").join("global.ini");
        fs::create_dir_all(path.parent().unwrap())?;
        let mut text = if path.exists() {
            fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };
        if !text.contains("[OBSWebSocket]") {
            text.push_str("\n[OBSWebSocket]\n");
        }
        text = set_ini_value(&text, "OBSWebSocket", "ServerEnabled", "true");
        text = set_ini_value(&text, "OBSWebSocket", "ServerPort", &port.to_string());
        text = set_ini_value(&text, "OBSWebSocket", "AuthRequired", "true");
        text = set_ini_value(&text, "OBSWebSocket", "ServerPassword", password);
        fs::write(path, text)?;
    }
    Ok(())
}
fn set_ini_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut out = Vec::<String>::new();
    let mut in_sec = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_sec && !done {
                out.push(format!("{}={}", key, value));
                done = true;
            }
            in_sec = trimmed == format!("[{}]", section);
            out.push(line.to_string());
            continue;
        }
        if in_sec
            && trimmed
                .split('=')
                .next()
                .map(|k| k.eq_ignore_ascii_case(key))
                .unwrap_or(false)
        {
            out.push(format!("{}={}", key, value));
            done = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !done {
        if !in_sec && !text.contains(&format!("[{}]", section)) {
            out.push(format!("[{}]", section));
        }
        out.push(format!("{}={}", key, value));
    }
    out.join("\n") + "\n"
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
    fn ini_writer_updates_existing_section_without_duplicates() {
        let original = "[General]\nName=Demo\n[OBSWebSocket]\nServerPort=4444\n";
        let updated = set_ini_value(original, "OBSWebSocket", "ServerPort", "4455");
        assert!(updated.contains("[OBSWebSocket]\nServerPort=4455\n"));
        assert!(!updated.contains("ServerPort=4444"));
        assert_eq!(updated.matches("ServerPort=").count(), 1);
    }

    #[test]
    fn ini_writer_creates_missing_section_and_key() {
        let updated = set_ini_value(
            "[General]\nName=Demo\n",
            "OBSWebSocket",
            "AuthRequired",
            "true",
        );
        assert!(updated.contains("[OBSWebSocket]\nAuthRequired=true\n"));
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
