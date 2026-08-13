//! Первичная настройка. Запускается один раз на каждой стороне.
//!
//! `--host` (компьютер актёра): ставит Tailscale и OBS, генерирует пароль
//! WebSocket, поднимает host-agent, прописывает автозапуск и показывает
//! pairing-код. После этого актёру больше ничего делать не нужно — при каждом
//! входе в Windows агент стартует сам и сам поднимает OBS.
//!
//! `--controller` (компьютер владельца): находит агента в tailnet и открывает панель.

use anyhow::{Context, Result, anyhow};
use remote_stream_control::*;
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<()> {
    ensure_dirs()?;
    init_logging()?;
    match std::env::args()
        .nth(1)
        .unwrap_or_else(|| "--help".into())
        .as_str()
    {
        "--host" => host_flow().await,
        "--controller" => controller_flow().await,
        "--remove-autostart" => {
            unregister_autostart()?;
            println!("Автозапуск Remote Stream Control удалён.");
            pause_if_console();
            Ok(())
        }
        _ => {
            println!(
                "Remote Stream Control bootstrap\n\n\
                 Использование:\n  \
                 bootstrap.exe --host              компьютер актёра/стримера\n  \
                 bootstrap.exe --controller        компьютер владельца\n  \
                 bootstrap.exe --remove-autostart  убрать агент из автозагрузки"
            );
            Ok(())
        }
    }
}

fn init_logging() -> Result<()> {
    let file = tracing_appender::rolling::never(logs_dir(), "bootstrap.log");
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

async fn host_flow() -> Result<()> {
    println!("Remote Stream Control — настройка компьютера актёра\n");
    let mut cfg = load_host_config()?;
    ensure_tailscale().await?;
    ensure_tailscale_up(cfg.enable_tailscale_unattended_after_login).await?;

    // Порядок важен: сначала OBS должен существовать, иначе мы настраиваем
    // ещё не установленный плагин и надеемся, что он подхватит наш файл.
    if cfg.auto_start_obs {
        ensure_obs_installed(&cfg).await?;
    }

    let obs_password = match load_secret("obs_websocket_password")? {
        Some(p) => p,
        None => {
            // Если OBS уже настроен, берём его пароль, а не навязываем свой:
            // на этот пароль могут быть настроены другие пульты у актёра.
            let p = match existing_obs_websocket_password() {
                Some(existing) => {
                    println!("OBS WebSocket: использую пароль, уже заданный в OBS");
                    existing
                }
                None => random_secret_b64(24),
            };
            save_secret("obs_websocket_password", &p)?;
            p
        }
    };
    configure_obs_websocket(&obs_password, cfg.obs_websocket_port).await?;

    // Пароль обязан жить только в DPAPI-хранилище. Если он попал в host.json
    // (например, его вписали руками), вычищаем файл.
    if !cfg.obs_websocket_password.is_empty() {
        cfg.obs_websocket_password.clear();
        save_json(&config_dir().join("host.json"), &cfg)?;
        println!("Пароль OBS убран из host.json — он хранится через DPAPI.");
    }

    // Запускаем только теперь, когда config.json уже лежит на диске: OBS
    // читает настройки плагина при старте.
    if cfg.auto_start_obs {
        match start_obs_if_needed(&cfg.obs_path) {
            Ok(true) => println!("OBS: запущен"),
            Ok(false) => println!("OBS: уже запущен"),
            Err(e) => println!("OBS: не удалось запустить — {e:#}"),
        }
    }
    wait_for_obs(&cfg, 25).await;
    ensure_host_agent_started().await?;
    wait_for_web(cfg.web_port, 20).await;

    match register_autostart() {
        Ok(path) => println!("Автозапуск настроен: {}", path.display()),
        Err(e) => println!(
            "Не удалось настроить автозапуск ({e:#}).\n\
             Ничего страшного: просто запускайте START_FRIEND.bat после входа в Windows."
        ),
    }

    let pairing = ensure_pairing_secret()?;
    println!("\n{}", "=".repeat(60));
    println!("Готово. Компьютер актёра настроен.");
    println!(
        "Панель владельца доступна через Tailscale на порту {}.",
        cfg.web_port
    );
    println!("\nPairing-код для владельца (сообщить один раз): {pairing}");
    println!("{}", "=".repeat(60));
    println!(
        "\nПри следующих включениях компьютера всё поднимется само —\n\
         запускать этот файл повторно не нужно.\n\
         Секреты не хранятся в JSON/BAT и защищены DPAPI Windows.\n"
    );
    pause_if_console();
    Ok(())
}

async fn controller_flow() -> Result<()> {
    println!("Remote Stream Control — запуск панели владельца\n");
    let cfg = load_controller_config()?;
    ensure_tailscale().await?;
    ensure_tailscale_up(true).await?;
    let targets = candidate_urls(&cfg);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let mut chosen = None;
    for url in &targets {
        print!("Проверяю {url} ... ");
        io::stdout().flush().ok();
        match client
            .get(format!("{}/api/public/ping", url.trim_end_matches('/')))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                println!("OK");
                chosen = Some(url.clone());
                break;
            }
            _ => println!("нет ответа"),
        }
    }
    let Some(url) = chosen else {
        println!(
            "\nНи один адрес не ответил. Проверьте:\n\
             1. Компьютер актёра включён и на нём хотя бы раз запускали START_FRIEND.bat.\n\
             2. Оба компьютера в одном tailnet (tailscale status).\n\
             3. friend_machine_name в config\\controller.json совпадает с именем машины в Tailscale.\n"
        );
        pause_if_console();
        return Err(anyhow!("host-agent недоступен"));
    };
    println!("Открываю панель: {url}");
    if cfg.auto_open_browser {
        open::that(&url).context("не удалось открыть браузер")?;
    }
    Ok(())
}

fn candidate_urls(cfg: &ControllerConfig) -> Vec<String> {
    let mut v = vec![format!(
        "http://{}:{}",
        cfg.friend_machine_name, cfg.web_port
    )];
    if !cfg.friend_machine_name.ends_with(".ts.net") {
        v.push(format!(
            "http://{}.local:{}",
            cfg.friend_machine_name, cfg.web_port
        ));
    }
    if !cfg.friend_tailscale_ip_fallback.trim().is_empty() {
        v.push(format!(
            "http://{}:{}",
            cfg.friend_tailscale_ip_fallback.trim(),
            cfg.web_port
        ));
    }
    v
}

async fn ensure_tailscale() -> Result<()> {
    if tailscale_exe().is_some() {
        println!("Tailscale: найден");
        return Ok(());
    }
    println!("Tailscale не найден. Устанавливаю официальный MSI...");
    let msi = installer_path("tailscale-setup-latest-amd64.msi");
    if !msi.exists() {
        download_tailscale(&msi).await?;
    }
    let status = Command::new("msiexec")
        .args([
            "/i",
            msi.to_str()
                .context("путь к MSI содержит недопустимые символы")?,
            "/passive",
            "/norestart",
        ])
        .status()?;
    if !status.success() {
        open::that("https://tailscale.com/download/windows")?;
        return Err(anyhow!(
            "Установите Tailscale в открывшемся окне и запустите START_FRIEND.bat ещё раз."
        ));
    }
    Ok(())
}

async fn download_tailscale(msi: &Path) -> Result<()> {
    fs::create_dir_all(msi.parent().context("нет родительской папки")?)?;
    let url = "https://pkgs.tailscale.com/stable/tailscale-setup-latest-amd64.msi";
    let bytes = reqwest::get(url).await?.bytes().await?;
    fs::write(msi, bytes)?;
    Ok(())
}

async fn ensure_tailscale_up(unattended: bool) -> Result<()> {
    let exe = tailscale_exe().ok_or_else(|| anyhow!("tailscale.exe не найден после установки"))?;
    if tailscale_running() {
        println!("Tailscale: подключён");
        return Ok(());
    }
    println!("Tailscale требует вход. Откроется стандартная страница авторизации.");
    let mut args = vec!["up".to_string()];
    if unattended {
        // Без этого флага туннель отваливается при выходе пользователя из системы.
        args.push("--unattended=true".into());
    }
    let out = Command::new(&exe).args(args).output()?;
    let txt =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    if let Some(url) = txt
        .split_whitespace()
        .find(|s| s.starts_with("http://") || s.starts_with("https://"))
    {
        let url = url.trim_matches(|c: char| c == '.' || c == ',');
        let _ = open::that(url);
        println!("Открыта ссылка входа Tailscale: {url}");
    }
    println!("Если вход только что выполнен, подождите пару секунд...");
    sleep(Duration::from_secs(3)).await;
    Ok(())
}

/// Включает WebSocket-сервер OBS.
///
/// Тонкость: OBS перезаписывает свой config.json при выходе из памяти. Если
/// писать настройки, пока OBS запущен, он затрёт их при закрытии, и пароль
/// не применится. Поэтому при необходимости изменений просим закрыть OBS
/// и ждём — запустим его потом сами.
async fn configure_obs_websocket(password: &str, port: u16) -> Result<()> {
    if !obs_websocket_config_matches(password, port) && obs_is_running() {
        println!("\nOBS запущен, а его настройки WebSocket нужно изменить.");
        println!("Закройте OBS — я подожду до 60 секунд и запущу его сам.");
        let deadline = Instant::now() + Duration::from_secs(60);
        while obs_is_running() && Instant::now() < deadline {
            print!(".");
            io::stdout().flush().ok();
            sleep(Duration::from_secs(1)).await;
        }
        println!();
        if obs_is_running() {
            println!(
                "OBS всё ещё запущен. Настройки записаны, но применятся\n\
                 только после того, как вы его перезапустите."
            );
        }
    }
    match ensure_obs_websocket_config(password, port)? {
        true => println!("OBS WebSocket: включён, пароль обновлён"),
        false => println!("OBS WebSocket: уже настроен"),
    }
    Ok(())
}

async fn ensure_obs_installed(cfg: &HostConfig) -> Result<()> {
    if find_obs(&cfg.obs_path).is_some() {
        println!("OBS Studio: найден");
        return Ok(());
    }
    println!("OBS Studio не найден. Скачиваю официальный установщик...");
    let installer = preferred_obs_installer_path();
    if !installer.exists() {
        let url = latest_obs_installer_url().await?;
        println!("Скачиваю OBS: {url}");
        let bytes = reqwest::Client::builder()
            .user_agent("RemoteStreamControl/0.2")
            .build()?
            .get(url)
            .send()
            .await?
            .bytes()
            .await?;
        fs::create_dir_all(
            installer
                .parent()
                .context("нет папки third_party/installers")?,
        )?;
        fs::write(&installer, bytes)?;
    }
    println!("Запускаю установку OBS. Если появится окно установщика — нажмите Install/Next.");
    let status = Command::new(&installer).arg("/S").status();
    sleep(Duration::from_secs(5)).await;
    if find_obs(&cfg.obs_path).is_none() {
        let _ = open::that("https://obsproject.com/download");
        return Err(anyhow!(
            "OBS Studio всё ещё не найден. Установите его вручную из открывшейся страницы и запустите START_FRIEND.bat ещё раз."
        ));
    }
    if let Ok(s) = status
        && !s.success()
    {
        println!("Установщик OBS завершился с ошибкой, но OBS найден — продолжаю.");
    }
    Ok(())
}

async fn latest_obs_installer_url() -> Result<String> {
    let v: serde_json::Value = reqwest::Client::builder()
        .user_agent("RemoteStreamControl/0.2")
        .build()?
        .get("https://api.github.com/repos/obsproject/obs-studio/releases/latest")
        .send()
        .await?
        .json()
        .await?;
    v.get("assets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .find_map(|a| {
            let name = a.get("name").and_then(|v| v.as_str())?.to_ascii_lowercase();
            let is_windows_installer = name.contains("windows") && name.ends_with("installer.exe");
            is_windows_installer.then(|| {
                a.get("browser_download_url")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })?
        })
        .ok_or_else(|| {
            anyhow!(
                "Не удалось найти установщик OBS для Windows в последнем релизе. \
                 Установите OBS вручную с https://obsproject.com/download"
            )
        })
}

async fn wait_for_obs(cfg: &HostConfig, seconds: u64) {
    print!("Жду OBS WebSocket");
    io::stdout().flush().ok();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        if tcp_ready(&cfg.obs_websocket_host, cfg.obs_websocket_port, 600).await {
            println!(" OK");
            return;
        }
        print!(".");
        io::stdout().flush().ok();
        sleep(Duration::from_secs(1)).await;
    }
    println!(" не дождался — панель покажет ошибку OBS");
}

async fn ensure_host_agent_started() -> Result<()> {
    if process_running("host-agent.exe") {
        println!("Host Agent: уже запущен");
        return Ok(());
    }
    let exe = bin_dir().join("host-agent.exe");
    if !exe.exists() {
        return Err(anyhow!("{} не найден", exe.display()));
    }
    println!("Запускаю Host Agent");
    Command::new(exe)
        .current_dir(app_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    sleep(Duration::from_secs(2)).await;
    Ok(())
}

async fn wait_for_web(port: u16, seconds: u64) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    print!("Проверяю web-панель");
    io::stdout().flush().ok();
    let start = Instant::now();
    let mut urls = vec![format!("http://127.0.0.1:{port}/api/public/ping")];
    if let Some(ip) = tailscale_ip() {
        urls.push(format!("http://{ip}:{port}/api/public/ping"));
    }
    while start.elapsed() < Duration::from_secs(seconds) {
        for url in &urls {
            if client
                .get(url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                println!(" OK");
                return;
            }
        }
        print!(".");
        io::stdout().flush().ok();
        sleep(Duration::from_secs(1)).await;
    }
    println!(" не дождался — смотрите logs\\host.log");
}

fn ensure_pairing_secret() -> Result<String> {
    match load_secret("pairing_secret")? {
        Some(s) => Ok(s),
        None => {
            let s = random_secret_b64(10);
            save_secret("pairing_secret", &s)?;
            Ok(s)
        }
    }
}

fn pause_if_console() {
    println!("Нажмите Enter, чтобы закрыть это окно.");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
}

fn installer_path(file_name: &str) -> PathBuf {
    app_root()
        .join("third_party")
        .join("installers")
        .join(file_name)
}

/// Ищем установщик, положенный в архив release-скриптом, чтобы не качать заново.
fn preferred_obs_installer_path() -> PathBuf {
    let installers = app_root().join("third_party").join("installers");
    if let Ok(entries) = fs::read_dir(&installers) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if name.starts_with("obs-studio-") && name.ends_with("-windows-installer.exe") {
                return path;
            }
        }
    }
    installers.join("OBS-Studio-Windows-Installer.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller_cfg(name: &str, fallback: &str) -> ControllerConfig {
        ControllerConfig {
            friend_machine_name: name.into(),
            friend_tailscale_ip_fallback: fallback.into(),
            web_port: 8787,
            auto_open_browser: false,
        }
    }

    #[test]
    fn magicdns_name_gets_local_variant() {
        let urls = candidate_urls(&controller_cfg("friend-pc", ""));
        assert_eq!(urls[0], "http://friend-pc:8787");
        assert_eq!(urls[1], "http://friend-pc.local:8787");
    }

    #[test]
    fn full_ts_net_name_skips_local_variant() {
        let urls = candidate_urls(&controller_cfg("friend-pc.tail1234.ts.net", ""));
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "http://friend-pc.tail1234.ts.net:8787");
    }

    #[test]
    fn ip_fallback_is_appended_last() {
        let urls = candidate_urls(&controller_cfg("friend-pc", " 100.64.0.5 "));
        assert_eq!(urls.last().unwrap(), "http://100.64.0.5:8787");
    }
}
