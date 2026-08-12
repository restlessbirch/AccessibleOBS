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
    let arg = std::env::args().nth(1).unwrap_or_else(|| "--help".into());
    match arg.as_str() {
        "--host" => host_flow().await,
        "--controller" => controller_flow().await,
        _ => {
            println!(
                "Remote Stream Control bootstrap\n\nИспользование:\n  bootstrap.exe --host        компьютер актёра/стримера\n  bootstrap.exe --controller  компьютер владельца"
            );
            Ok(())
        }
    }
}

fn init_logging() -> Result<()> {
    let file = tracing_appender::rolling::never(logs_dir(), "bootstrap.log");
    let (writer, _guard) = tracing_appender::non_blocking(file);
    std::mem::forget(_guard);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .ok();
    Ok(())
}

async fn host_flow() -> Result<()> {
    println!("Remote Stream Control — запуск компьютера актёра\n");
    let mut cfg = load_host_config()?;
    ensure_tailscale().await?;
    ensure_tailscale_up(cfg.enable_tailscale_unattended_after_login).await?;

    let obs_password = match load_secret("obs_websocket_password")? {
        Some(p) => p,
        None => {
            let p = random_secret_b64(24);
            save_secret("obs_websocket_password", &p)?;
            p
        }
    };
    ensure_obs_websocket_config(&obs_password, cfg.obs_websocket_port)?;
    if cfg.obs_websocket_password.is_empty() {
        cfg.obs_websocket_password = String::new();
        save_json(&config_dir().join("host.json"), &cfg)?;
    }

    if cfg.auto_start_obs {
        ensure_obs_installed(&cfg).await?;
        ensure_obs_started(&cfg).await?;
    }
    wait_for_obs(&cfg, 25).await;
    ensure_host_agent_started().await?;
    wait_for_web(cfg.web_port, 20).await;

    let pairing = ensure_pairing_secret()?;
    println!(
        "\nГотово. Панель владельца должна открываться через Tailscale на порту {}.",
        cfg.web_port
    );
    println!(
        "Если владелец видит экран pairing, одноразово введите этот код: {}",
        pairing
    );
    println!("Секреты не записаны в JSON/BAT и защищены DPAPI Windows. Это окно можно свернуть.\n");
    pause_if_console();
    Ok(())
}

async fn controller_flow() -> Result<()> {
    println!("Remote Stream Control — запуск владельца\n");
    let cfg = load_controller_config()?;
    ensure_tailscale().await?;
    ensure_tailscale_up(true).await?;
    let targets = candidate_urls(&cfg);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let mut chosen = None;
    for url in &targets {
        print!("Проверяю {} ... ", url);
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
    let url = chosen.unwrap_or_else(|| targets[0].clone());
    println!("Открываю панель: {}", url);
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

fn tailscale_exe() -> Option<PathBuf> {
    find_exe("tailscale.exe").or_else(|| {
        let p = PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
        p.exists().then_some(p)
    })
}
async fn ensure_tailscale() -> Result<()> {
    if tailscale_exe().is_some() {
        println!("Tailscale: найден");
        return Ok(());
    }
    println!("Tailscale не найден. Пробую установить официальный MSI...");
    let msi = installer_path("tailscale-setup-latest-amd64.msi");
    if !msi.exists() {
        download_tailscale(&msi).await?;
    }
    let status = Command::new("msiexec")
        .args(["/i", msi.to_str().unwrap(), "/passive", "/norestart"])
        .status()?;
    if !status.success() {
        open::that("https://tailscale.com/download/windows")?;
        return Err(anyhow!(
            "Установите Tailscale в открывшемся окне и запустите BAT ещё раз."
        ));
    }
    Ok(())
}
async fn download_tailscale(msi: &Path) -> Result<()> {
    fs::create_dir_all(msi.parent().unwrap())?;
    let url = "https://pkgs.tailscale.com/stable/tailscale-setup-latest-amd64.msi";
    let bytes = reqwest::get(url).await?.bytes().await?;
    fs::write(msi, bytes)?;
    Ok(())
}
async fn ensure_tailscale_up(unattended: bool) -> Result<()> {
    let exe = tailscale_exe().ok_or_else(|| anyhow!("tailscale.exe не найден после установки"))?;
    let status = Command::new(&exe).args(["status", "--json"]).output();
    let mut running = false;
    if let Ok(out) = status {
        let txt = String::from_utf8_lossy(&out.stdout);
        running = txt.contains("\"BackendState\":\"Running\"")
            || txt.contains("\"BackendState\": \"Running\"");
    }
    if running {
        println!("Tailscale: подключён");
        return Ok(());
    }
    println!("Tailscale требует вход. Откроется стандартная авторизация Tailscale.");
    let mut args = vec!["up".to_string()];
    if unattended {
        args.push("--unattended=true".into());
    }
    let out = Command::new(&exe).args(args).output()?;
    let txt =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    if let Some(url) = txt
        .split_whitespace()
        .find(|s| s.starts_with("http://") || s.starts_with("https://"))
    {
        let _ = open::that(url.trim_matches(|c: char| c == '.' || c == ','));
        println!("Открыта ссылка входа Tailscale: {}", url);
    }
    println!("Если авторизация только что выполнена, подождите пару секунд...");
    sleep(Duration::from_secs(3)).await;
    Ok(())
}

async fn ensure_obs_installed(cfg: &HostConfig) -> Result<()> {
    if find_obs(&cfg.obs_path).is_some() {
        println!("OBS Studio: найден");
        return Ok(());
    }
    println!("OBS Studio не найден. Пробую скачать официальный installer OBS...");
    let installer = preferred_obs_installer_path();
    if !installer.exists() {
        let url = latest_obs_installer_url().await.unwrap_or_else(|_| "https://cdn-fastly.obsproject.com/downloads/OBS-Studio-31.1.2-Windows-Installer.exe".to_string());
        println!("Скачиваю OBS: {}", url);
        let bytes = reqwest::Client::builder()
            .user_agent("RemoteStreamControl/1.0")
            .build()?
            .get(url)
            .send()
            .await?
            .bytes()
            .await?;
        fs::create_dir_all(installer.parent().unwrap_or(Path::new("third_party")))?;
        fs::write(&installer, bytes)?;
    }
    println!("Запускаю установку OBS. Если появится окно установщика — нажмите Install/Next.");
    let status = Command::new(&installer).arg("/S").status();
    sleep(Duration::from_secs(5)).await;
    if find_obs(&cfg.obs_path).is_none() {
        let _ = open::that("https://obsproject.com/download");
        return Err(anyhow!(
            "OBS Studio пока не найден. Установите OBS из открывшегося окна/страницы и запустите START_FRIEND.bat ещё раз."
        ));
    }
    if let Ok(s) = status
        && !s.success()
    {
        println!("OBS installer завершился не идеально, но OBS найден — продолжаю.");
    }
    Ok(())
}
async fn latest_obs_installer_url() -> Result<String> {
    let v: serde_json::Value = reqwest::Client::builder()
        .user_agent("RemoteStreamControl/1.0")
        .build()?
        .get("https://api.github.com/repos/obsproject/obs-studio/releases/latest")
        .send()
        .await?
        .json()
        .await?;
    let assets = v
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("OBS GitHub release assets missing"))?;
    for a in assets {
        let name = a
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("windows")
            && name.ends_with("installer.exe")
            && let Some(url) = a.get("browser_download_url").and_then(|v| v.as_str())
        {
            return Ok(url.to_string());
        }
    }
    Err(anyhow!("OBS Windows installer asset not found"))
}
async fn ensure_obs_started(cfg: &HostConfig) -> Result<()> {
    if process_running("obs64.exe") || process_running("obs.exe") {
        println!("OBS: уже запущен");
        return Ok(());
    }
    let obs=find_obs(&cfg.obs_path).ok_or_else(|| anyhow!("OBS Studio не найден. Установите OBS Studio 28+ с https://obsproject.com/ и запустите BAT ещё раз."))?;
    println!("Запускаю OBS: {}", obs.display());
    let mut cmd = Command::new(&obs);
    if let Some(parent) = obs.parent() {
        cmd.current_dir(parent);
    }
    cmd.arg("--minimize-to-tray")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
fn process_running(name: &str) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {}", name)])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .contains(&name.to_ascii_lowercase())
        })
        .unwrap_or(false)
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
    println!(" не дождался, панель покажет ошибку OBS");
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
        .unwrap();
    print!("Проверяю web-панель");
    io::stdout().flush().ok();
    let start = Instant::now();
    let mut urls = vec![format!("http://127.0.0.1:{}/api/public/ping", port)];
    if let Some(ip) = current_tailscale_ip() {
        urls.push(format!("http://{}:{}/api/public/ping", ip, port));
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
    println!(" не дождался, смотрите logs\\host.log");
}
fn current_tailscale_ip() -> Option<String> {
    let exe = tailscale_exe()?;
    let out = Command::new(exe).arg("ip").arg("-4").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn ensure_pairing_secret() -> Result<String> {
    if let Some(s) = load_secret("pairing_secret")? {
        Ok(s)
    } else {
        let s = random_secret_b64(10);
        save_secret("pairing_secret", &s)?;
        Ok(s)
    }
}
fn pause_if_console() {
    println!("Нажмите Enter, чтобы закрыть это окно, или просто сверните его.");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
}

fn installer_path(file_name: &str) -> PathBuf {
    app_root()
        .join("third_party")
        .join("installers")
        .join(file_name)
}

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
