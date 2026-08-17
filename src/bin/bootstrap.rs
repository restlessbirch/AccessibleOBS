//! Первичная настройка. Запускается один раз на каждой стороне.
//!
//! `--host` (компьютер актёра): ставит Tailscale и OBS, генерирует пароль
//! WebSocket, поднимает host-agent, прописывает автозапуск и показывает
//! pairing-код. После этого актёру больше ничего делать не нужно — при каждом
//! входе в Windows агент стартует сам и сам поднимает OBS.
//!
//! `--controller` (компьютер владельца): находит агента в tailnet и открывает панель.

use accessible_obs::*;
use anyhow::{Context, Result, anyhow};
use axum::{
    Form, Router,
    extract::State,
    http::HeaderMap,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use serde_json::Value;
use std::{
    fs,
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tokio::time::sleep;

const LAUNCHER_PORT: u16 = 8786;

#[tokio::main]
async fn main() -> Result<()> {
    ensure_dirs()?;
    // Держим страж до конца main: он живёт столько же, сколько процесс.
    let _log_guard = init_file_logging("bootstrap.log")?;
    match std::env::args()
        .nth(1)
        .unwrap_or_else(|| "--launcher".into())
        .as_str()
    {
        "--launcher" | "--install" => launcher_flow().await,
        "--host" => host_flow().await,
        "--controller" => controller_flow().await,
        "--local" => local_flow().await,
        "--remove-autostart" => {
            unregister_autostart()?;
            println!("Автозапуск Accessible OBS удалён.");
            pause_if_console();
            Ok(())
        }
        _ => {
            println!(
                "Accessible OBS bootstrap\n\n\
                 Использование:\n  \
                 AccessibleOBS.exe                   открыть доступное меню запуска\n  \
                 AccessibleOBS.exe --launcher        открыть доступное меню запуска\n  \
                 AccessibleOBS.exe --host              компьютер актёра/стримера\n  \
                 AccessibleOBS.exe --controller        компьютер владельца\n  \
                 AccessibleOBS.exe --local             локальный доступный режим\n  \
                 AccessibleOBS.exe --remove-autostart  убрать агент из автозагрузки"
            );
            Ok(())
        }
    }
}

async fn launcher_flow() -> Result<()> {
    let shortcut = register_launcher_shortcut()?;
    println!(
        "Accessible OBS launcher\n\n\
         Desktop shortcut: {}\n\
         Opening accessible launcher...",
        shortcut.display()
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], LAUNCHER_PORT));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(_) => {
            let url = launcher_url();
            println!("Launcher already seems to be running. Opening {url}");
            open::that(url).context("не удалось открыть браузер")?;
            return Ok(());
        }
    };

    let state = LauncherState {
        exe: Arc::new(launcher_exe()),
        // Новый на каждый запуск: страница, оставшаяся в чужой вкладке с
        // прошлого раза, действовать уже не сможет.
        nonce: Arc::new(random_secret_b64(24)),
    };
    let app = Router::new()
        .route("/", get(launcher_page))
        .route("/actor", post(launcher_actor))
        .route("/operator", post(launcher_operator))
        .route("/local", post(launcher_local))
        .route("/interface-mode/accessible", post(launcher_mode_accessible))
        .route("/interface-mode/standard", post(launcher_mode_standard))
        .route("/remove-autostart", post(launcher_remove_autostart))
        .with_state(state);

    let url = launcher_url();
    open::that(&url).context("не удалось открыть браузер")?;
    println!("Launcher: {url}");
    println!("Keep this window open while using the launcher.");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Clone)]
struct LauncherState {
    exe: Arc<PathBuf>,
    /// Одноразовый пароль страницы, выдаваемый только ей самой.
    ///
    /// Начальная страница умеет запускать программы и снимать автозапуск, и до сих пор
    /// принимала POST от кого угодно. Чужой открытой вкладке даже не нужно
    /// читать ответ: обычной html-формы хватает, чтобы отправить запрос на
    /// localhost, пока страница запущена. Проверки Origin мало — она не спасёт,
    /// если страницу встроят фреймом, поэтому вместе с ней требуем значение,
    /// которое чужой странице взять неоткуда.
    nonce: Arc<String>,
}

/// Поля, приходящие с любой формы начальной страницы.
#[derive(serde::Deserialize)]
struct LauncherForm {
    nonce: String,
}

/// Пускать ли действие: и происхождение своё, и пароль страницы совпал.
fn launcher_allowed(st: &LauncherState, headers: &HeaderMap, form: &LauncherForm) -> bool {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let host = headers.get("host").and_then(|v| v.to_str().ok());
    let from_us = match origin {
        Some(origin) => loopback_request_ok(Some(origin), None),
        None => loopback_request_ok(None, host),
    };
    from_us && secret_eq(&form.nonce, &st.nonce)
}

/// Сравнение за постоянное время: обычное сравнение выходит на первом
/// различии и позволяет подбирать значение по времени ответа.
fn secret_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn launcher_refused() -> Html<String> {
    Html(result_page(
        "Действие отклонено",
        "Запрос пришёл не со страницы запуска. Откройте её заново и повторите.",
        Some(&launcher_url()),
    ))
}

fn launcher_url() -> String {
    format!("http://127.0.0.1:{LAUNCHER_PORT}/")
}

fn launcher_exe() -> PathBuf {
    let root_exe = app_root().join("AccessibleOBS.exe");
    if root_exe.exists() {
        root_exe
    } else {
        std::env::current_exe().unwrap_or_else(|_| bin_dir().join("bootstrap.exe"))
    }
}

async fn launcher_page(State(st): State<LauncherState>) -> Html<String> {
    // Показываем текущий выбор прямо на странице: незрячий иначе не поймёт,
    // в каком режиме окажется, пока не запустит панель.
    let mode = load_host_config()
        .map(|c| c.interface_mode)
        .unwrap_or(InterfaceMode::Accessible);
    let label = match mode {
        InterfaceMode::Accessible => "доступный, для незрячего",
        InterfaceMode::Standard => "обычный, для зрячего",
    };
    Html(
        LAUNCHER_HTML
            .replace("{MODE}", label)
            .replace("{NONCE}", &html_escape(&st.nonce)),
    )
}

async fn launcher_mode_accessible(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    set_interface_mode(InterfaceMode::Accessible)
}

async fn launcher_mode_standard(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    set_interface_mode(InterfaceMode::Standard)
}

fn set_interface_mode(mode: InterfaceMode) -> Html<String> {
    let outcome = load_host_config().and_then(|mut cfg| {
        cfg.interface_mode = mode;
        save_json(&config_dir().join("host.json"), &cfg)
    });
    match outcome {
        Ok(()) => Html(result_page(
            "Режим сохранён",
            match mode {
                InterfaceMode::Accessible => {
                    "Выбран доступный режим: важное зачитывается вслух, \
                     вывод на второй монитор скрыт."
                }
                InterfaceMode::Standard => {
                    "Выбран обычный режим: доступен вывод на второй монитор, \
                     вслух ничего не зачитывается."
                }
            },
            Some(&launcher_url()),
        )),
        Err(e) => Html(result_page(
            "Не удалось сохранить режим",
            &e.to_string(),
            Some(&launcher_url()),
        )),
    }
}

async fn launcher_actor(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    spawn_launcher_command(&st.exe, "--host", "Actor setup")
}

async fn launcher_operator(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    spawn_launcher_command(&st.exe, "--controller", "Operator panel")
}

async fn launcher_local(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    spawn_launcher_command(&st.exe, "--local", "Local accessible mode")
}

async fn launcher_remove_autostart(
    State(st): State<LauncherState>,
    headers: HeaderMap,
    Form(form): Form<LauncherForm>,
) -> impl IntoResponse {
    if !launcher_allowed(&st, &headers, &form) {
        return launcher_refused();
    }
    match unregister_autostart() {
        Ok(()) => Html(result_page(
            "Autostart removed",
            "Remote actor autostart shortcut was removed.",
            Some(&launcher_url()),
        )),
        Err(e) => Html(result_page(
            "Autostart error",
            &e.to_string(),
            Some(&launcher_url()),
        )),
    }
}

fn spawn_launcher_command(exe: &Path, arg: &str, title: &str) -> Html<String> {
    match Command::new(exe).arg(arg).current_dir(app_root()).spawn() {
        Ok(_) => Html(result_page(
            title,
            "A separate setup window was opened. Follow the spoken or on-screen instructions there.",
            Some(&launcher_url()),
        )),
        Err(e) => Html(result_page(
            title,
            &format!("Could not start command: {e}"),
            Some(&launcher_url()),
        )),
    }
}

fn result_page(title: &str, message: &str, back: Option<&str>) -> String {
    let back_link = back
        .map(|url| format!(r#"<p><a href="{url}">Back to launcher</a></p>"#))
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{}</title>
  <style>{}</style>
</head>
<body>
  <main>
    <h1>{}</h1>
    <p role="status">{}</p>
    {}
  </main>
</body>
</html>"#,
        html_escape(title),
        LAUNCHER_CSS,
        html_escape(title),
        html_escape(message),
        back_link
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const LAUNCHER_CSS: &str = r#"
:root { color-scheme: light dark; }
body {
  margin: 0;
  font-family: system-ui, "Segoe UI", sans-serif;
  line-height: 1.5;
  background: Canvas;
  color: CanvasText;
}
main {
  max-width: 760px;
  margin: 0 auto;
  padding: 24px;
}
h1 { font-size: 1.8rem; margin: 0 0 12px; }
p { margin: 0 0 18px; }
form { margin: 16px 0; }
button, a {
  font: inherit;
  min-height: 48px;
}
button {
  width: 100%;
  text-align: left;
  border: 2px solid ButtonText;
  background: ButtonFace;
  color: ButtonText;
  padding: 14px 16px;
  border-radius: 6px;
  cursor: pointer;
}
button:focus-visible, a:focus-visible {
  outline: 3px solid Highlight;
  outline-offset: 3px;
}
.hint {
  display: block;
  margin-top: 4px;
  font-size: 0.95rem;
}
"#;

const LAUNCHER_HTML: &str = r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Accessible OBS</title>
  <style>
:root { color-scheme: light dark; }
body {
  margin: 0;
  font-family: system-ui, "Segoe UI", sans-serif;
  line-height: 1.5;
  background: Canvas;
  color: CanvasText;
}
main {
  max-width: 760px;
  margin: 0 auto;
  padding: 24px;
}
h1 { font-size: 1.8rem; margin: 0 0 12px; }
p { margin: 0 0 18px; }
form { margin: 16px 0; }
button {
  width: 100%;
  min-height: 56px;
  text-align: left;
  border: 2px solid ButtonText;
  background: ButtonFace;
  color: ButtonText;
  padding: 14px 16px;
  border-radius: 6px;
  font: inherit;
  cursor: pointer;
}
button:focus-visible {
  outline: 3px solid Highlight;
  outline-offset: 3px;
}
.hint {
  display: block;
  margin-top: 4px;
  font-size: 0.95rem;
}
  </style>
</head>
<body>
  <main>
    <h1>Accessible OBS</h1>
    <p>Выберите, как использовать этот компьютер.</p>

    <h2>Режим интерфейса</h2>
    <p>
      От него зависит, что панель показывает и что произносит вслух.
      Сейчас выбран: <strong>{MODE}</strong>
    </p>

    <form method="post" action="/interface-mode/accessible">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Доступный: для незрячего
        <span class="hint">Чат, донаты и тревоги зачитываются вслух. Вывод на второй монитор скрыт: окно проектора OBS экранный диктор прочитать не может в принципе.</span>
      </button>
    </form>

    <form method="post" action="/interface-mode/standard">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Обычный: для зрячего
        <span class="hint">Доступен вывод чата и донатов на второй монитор через проектор OBS. Вслух ничего не зачитывается.</span>
      </button>
    </form>

    <h2>Что запустить</h2>

    <form method="post" action="/actor">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Актёр: настроить компьютер для стрима
        <span class="hint">Установит или скачает OBS и Tailscale, включит агент, покажет pairing-код.</span>
      </button>
    </form>

    <form method="post" action="/operator">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Оператор: открыть панель управления
        <span class="hint">Откроет удалённую панель через Tailscale и попросит pairing-код.</span>
      </button>
    </form>

    <form method="post" action="/local">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Локальный доступный режим
        <span class="hint">Для незрячего стримера на этом же компьютере, без Tailscale и pairing-кода.</span>
      </button>
    </form>

    <form method="post" action="/remove-autostart">
      <input type="hidden" name="nonce" value="{NONCE}">
      <button type="submit">
        Убрать автозапуск удалённого агента
        <span class="hint">Полезно, если локальный режим конфликтует с remote-режимом на порту 8787.</span>
      </button>
    </form>
  </main>
</body>
</html>"#;

async fn host_flow() -> Result<()> {
    println!("Accessible OBS — настройка компьютера актёра\n");
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

async fn local_flow() -> Result<()> {
    println!("Accessible OBS — локальный доступный режим\n");
    let cfg = load_host_config()?;
    ensure_obs_installed(&cfg).await?;
    let obs_password = runtime_or_existing_obs_password(&cfg)?;
    configure_obs_websocket(&obs_password, cfg.obs_websocket_port).await?;

    let url = format!("http://127.0.0.1:{}/", cfg.web_port);
    match public_ping_mode(cfg.web_port).await {
        Some(mode) if mode == "local" => {
            println!("Локальный режим уже работает: {url}");
            open::that(&url).context("не удалось открыть браузер")?;
            return Ok(());
        }
        Some(mode) if mode == "remote" => {
            println!(
                "На порту {} уже работает удалённый агент.\n\
                 Уберите автозапуск через кнопку в меню или командой:\n\
                 bootstrap.exe --remove-autostart\n\
                 Затем завершите host-agent.exe и запустите локальный режим снова.",
                cfg.web_port
            );
            pause_if_console();
            return Err(anyhow!("порт занят удалённым агентом"));
        }
        _ => {}
    }

    if cfg.auto_start_obs {
        match start_obs_if_needed(&cfg.obs_path) {
            Ok(true) => println!("OBS: запущен"),
            Ok(false) => println!("OBS: уже запущен"),
            Err(e) => println!("OBS: не удалось запустить — {e:#}"),
        }
    }
    wait_for_obs(&cfg, 25).await;
    ensure_local_host_agent_started().await?;
    wait_for_local_web(cfg.web_port, 20).await?;
    create_launcher_shortcut(
        "Accessible OBS - Local.lnk",
        &launcher_exe(),
        "--local",
        "Accessible OBS local accessible mode",
    )
    .ok();
    println!("Открываю локальную панель: {url}");
    open::that(&url).context("не удалось открыть браузер")?;
    Ok(())
}

async fn controller_flow() -> Result<()> {
    println!("Accessible OBS — запуск панели владельца\n");
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
            // Успешного кода мало. На порту 8787 может отвечать что угодно
            // чужое: ошибка в имени машины, устаревшая запись DNS, соседний
            // сервис. Человека нельзя вести в чужую панель, поэтому спрашиваем
            // отклик и убеждаемся, что это именно наш агент.
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) if v.get("app").and_then(Value::as_str) == Some(APP_NAME) => {
                    let mode = v.get("mode").and_then(Value::as_str).unwrap_or("");
                    println!("OK, режим {mode}");
                    chosen = Some(url.clone());
                    break;
                }
                Ok(_) => println!("отвечает чужая программа, пропускаю"),
                Err(_) => println!("отклик не разобран, пропускаю"),
            },
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

async fn public_ping_mode(port: u16) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/api/public/ping");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let value: Value = client.get(url).send().await.ok()?.json().await.ok()?;
    value
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_string)
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
    let manifest = load_installer_manifest()?;
    let msi = pinned_installer(&manifest.tailscale).await?;
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

/// Отдаёт путь к установщику той версии, что записана в манифесте.
///
/// Готовый файл из архива тоже проверяется, а не принимается на веру: с
/// момента сборки релиза он лежал на диске и мог быть подменён.
///
/// Скачиваем строго по ссылке из манифеста. Прежде здесь стоял адрес с
/// «latest» в имени: он ведёт каждый раз на разный файл, а значит проверить
/// его нечем в принципе.
async fn pinned_installer(entry: &InstallerEntry) -> Result<PathBuf> {
    let path = installers_dir().join(&entry.file);
    if !path.exists() {
        println!("Скачиваю {} {}...", entry.file, entry.version);
        fs::create_dir_all(installers_dir())?;
        let bytes = reqwest::Client::builder()
            .user_agent(concat!("AccessibleOBS/", env!("CARGO_PKG_VERSION")))
            .build()?
            .get(&entry.url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        fs::write(&path, bytes)?;
    }
    match verify_installer(&path, entry) {
        Ok(()) => {
            println!("{}: контрольная сумма совпала", entry.file);
            Ok(path)
        }
        Err(e) => {
            // Испорченный файл убираем, иначе следующий запуск наткнётся на
            // него снова и снова будет отказывать.
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
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
    let manifest = load_installer_manifest()?;
    let installer = pinned_installer(&manifest.obs).await?;
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

fn runtime_or_existing_obs_password(cfg: &HostConfig) -> Result<String> {
    if let Some(secret) = load_secret("obs_websocket_password")? {
        return Ok(secret);
    }
    if !cfg.obs_websocket_password.trim().is_empty() {
        let password = cfg.obs_websocket_password.clone();
        save_secret("obs_websocket_password", &password)?;
        return Ok(password);
    }
    if let Some(existing) = existing_obs_websocket_password() {
        save_secret("obs_websocket_password", &existing)?;
        return Ok(existing);
    }
    let password = random_secret_b64(24);
    save_secret("obs_websocket_password", &password)?;
    Ok(password)
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

async fn ensure_local_host_agent_started() -> Result<()> {
    let exe = bin_dir().join("host-agent.exe");
    if !exe.exists() {
        return Err(anyhow!("{} не найден", exe.display()));
    }
    println!("Запускаю Host Agent в локальном режиме");
    Command::new(exe)
        .current_dir(app_root())
        .arg("--local")
        .arg("--no-open")
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

async fn wait_for_local_web(port: u16, seconds: u64) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        if public_ping_mode(port).await.as_deref() == Some("local") {
            println!("Локальная web-панель: OK");
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    Err(anyhow!("локальная web-панель не ответила"))
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

fn register_launcher_shortcut() -> Result<PathBuf> {
    create_launcher_shortcut(
        "Accessible OBS.lnk",
        &launcher_exe(),
        "",
        "Accessible OBS accessible launcher",
    )
}

fn create_launcher_shortcut(
    file_name: &str,
    target: &Path,
    arguments: &str,
    description: &str,
) -> Result<PathBuf> {
    let desktop = desktop_dir().context("не удалось определить рабочий стол")?;
    fs::create_dir_all(&desktop)?;
    let shortcut = desktop.join(file_name);
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.Arguments = '{}'; \
         $s.WorkingDirectory = '{}'; \
         $s.Description = '{}'; \
         $s.Save()",
        ps_quote(&shortcut.to_string_lossy()),
        ps_quote(&target.to_string_lossy()),
        ps_quote(arguments),
        ps_quote(&app_root().to_string_lossy()),
        ps_quote(description),
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("не удалось запустить powershell для создания ярлыка")?;
    if !shortcut.exists() {
        return Err(anyhow!(
            "ярлык не создан: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(shortcut)
}

fn desktop_dir() -> Option<PathBuf> {
    let profile = std::env::var("USERPROFILE").ok()?;
    let onedrive = PathBuf::from(&profile).join("OneDrive").join("Desktop");
    if onedrive.exists() {
        return Some(onedrive);
    }
    Some(PathBuf::from(profile).join("Desktop"))
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

    // Проверки угадывания имени установщика убраны вместе с самим угадыванием:
    // имя файла теперь берётся из манифеста, где оно записано вместе с версией
    // и контрольной суммой. Искать установщик по образцу имени больше не нужно,
    // а тест на несуществующую функцию — мусор.

    #[test]
    fn launcher_accepts_only_its_own_page() {
        // Действия начальной страницы запускают программы и снимают
        // автозапуск, поэтому чужой POST на localhost проходить не должен.
        let st = LauncherState {
            exe: Arc::new(PathBuf::from("x")),
            nonce: Arc::new("правильный".to_string()),
        };
        let mut own = HeaderMap::new();
        own.insert("origin", "http://127.0.0.1:8786".parse().unwrap());

        assert!(launcher_allowed(
            &st,
            &own,
            &LauncherForm {
                nonce: "правильный".into()
            }
        ));
        // Своя страница, но пароль не тот — например, страница осталась
        // открытой с прошлого запуска программы.
        assert!(!launcher_allowed(
            &st,
            &own,
            &LauncherForm {
                nonce: "устаревший".into()
            }
        ));

        let mut foreign = HeaderMap::new();
        foreign.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!launcher_allowed(
            &st,
            &foreign,
            &LauncherForm {
                nonce: "правильный".into()
            }
        ));
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
