//! Проверка готовности к эфиру.
//!
//! «OBS подключён» — не то же самое, что «можно начинать». Сцена может быть
//! пустой, микрофон заглушен, диск полон, а оверлей донатов перекрыт игрой.
//! Зрячий стример увидит это в окне OBS за секунду; незрячему оператору
//! видеть нечем, и узнать он рискует от зрителей.
//!
//! Отдельная причина существования этого модуля — звук рабочего стола.
//! Если он попадает в эфир, вместе с игрой туда идёт и речь экранного
//! диктора: зрители слышат, как программа зачитывает оператору интерфейс.
//! Сам OBS об этом не предупреждает и знать об этом не может.
//!
//! Логика чистая: на вход снимок состояния, на выход перечень проверок.
//! Обращений к OBS здесь нет, поэтому всё покрывается тестами.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Всё в порядке.
    Ok,
    /// Эфир возможен, но, скорее всего, это не то, чего хотел оператор.
    Warning,
    /// Начинать нельзя: зрители получат заведомо испорченный эфир.
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub title: String,
    pub severity: Severity,
    pub detail: String,
    /// Что сделать. Пусто, когда делать нечего.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn ok(title: &str, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            severity: Severity::Ok,
            detail: detail.into(),
            fix: None,
        }
    }
    fn warn(title: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            severity: Severity::Warning,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    fn critical(title: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            severity: Severity::Critical,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AudioInput {
    pub name: String,
    /// Роль по версии самого OBS: "mic" или "desktop".
    pub role: Option<String>,
    pub muted: bool,
    /// Последний измеренный пик, dB. None — измерений ещё не было.
    pub level_db: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct SceneSource {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OverlayState {
    pub present_in_scenes: bool,
    pub on_top: bool,
    pub muted: bool,
}

/// Снимок состояния, по которому выносится вердикт.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub obs_connected: bool,
    pub obs_version: Option<String>,
    pub current_scene: Option<String>,
    pub sources: Vec<SceneSource>,
    pub audio: Vec<AudioInput>,
    pub free_disk_mb: f64,
    pub streaming: bool,
    pub recording: bool,
    /// Настроен ли сервис вещания (сервер и ключ потока).
    pub stream_service_configured: bool,
    /// Состояние оверлея донатов. None — DonationAlerts не настраивался.
    pub donation_overlay: Option<OverlayState>,
    pub twitch_connected: bool,
}

/// Ниже этого уровня считаем, что звука нет.
const SILENT_DB: f64 = -50.0;
/// Меньше этого объёма запись рискует оборваться на середине.
const LOW_DISK_MB: f64 = 5000.0;

pub fn evaluate(snapshot: &Snapshot) -> Vec<Check> {
    let mut checks = Vec::new();

    if !snapshot.obs_connected {
        // Без связи остальные проверки бессмысленны: данных для них нет.
        checks.push(Check::critical(
            "OBS",
            "Нет связи с OBS",
            "Нажмите «Запустить OBS» или попросите актёра открыть OBS.",
        ));
        return checks;
    }
    checks.push(Check::ok(
        "OBS",
        match &snapshot.obs_version {
            Some(v) => format!("Подключён, версия {v}"),
            None => "Подключён".to_string(),
        },
    ));

    checks.push(match &snapshot.current_scene {
        Some(scene) => Check::ok("Текущая сцена", scene.clone()),
        None => Check::critical(
            "Текущая сцена",
            "OBS не сообщил текущую сцену",
            "Создайте хотя бы одну сцену в OBS.",
        ),
    });

    checks.push(scene_content_check(&snapshot.sources));
    checks.extend(microphone_checks(&snapshot.audio));
    checks.push(desktop_audio_check(&snapshot.audio));

    if let Some(overlay) = &snapshot.donation_overlay {
        checks.push(donation_check(overlay));
    }

    checks.push(if snapshot.stream_service_configured {
        Check::ok("Сервис вещания", "Сервер и ключ потока заданы")
    } else {
        Check::critical(
            "Сервис вещания",
            "В OBS не задан ключ потока",
            "У актёра: Настройки OBS → Вещание → выбрать сервис и вставить ключ.",
        )
    });

    checks.push(disk_check(snapshot.free_disk_mb, snapshot.recording));

    checks.push(if snapshot.twitch_connected {
        Check::ok("Twitch", "Подключён")
    } else {
        Check::warn(
            "Twitch",
            "Не подключён",
            "Без него нельзя менять название трансляции и категорию из панели.",
        )
    });

    checks
}

fn scene_content_check(sources: &[SceneSource]) -> Check {
    if sources.is_empty() {
        return Check::critical(
            "Содержимое сцены",
            "В текущей сцене нет источников — зрители увидят чёрный экран",
            "Добавьте источник: захват игры, экрана или камеру.",
        );
    }
    let visible = sources.iter().filter(|s| s.enabled).count();
    if visible == 0 {
        return Check::critical(
            "Содержимое сцены",
            format!(
                "Все {} источников скрыты — в эфир пойдёт чёрный экран",
                sources.len()
            ),
            "Включите нужный источник в разделе «Источники».",
        );
    }
    Check::ok(
        "Содержимое сцены",
        format!("Видимых источников: {visible} из {}", sources.len()),
    )
}

fn microphone_checks(audio: &[AudioInput]) -> Vec<Check> {
    let Some(mic) = audio.iter().find(|a| a.role.as_deref() == Some("mic")) else {
        return vec![Check::critical(
            "Микрофон",
            "OBS не знает, какой источник считать микрофоном",
            "У актёра: Настройки OBS → Аудио → «Микрофон/дополнительное аудио». \
             Без этого не работает и горячая клавиша заглушения.",
        )];
    };

    let mut checks = vec![if mic.muted {
        Check::critical(
            "Микрофон",
            format!("{} заглушен — вас не будет слышно", mic.name),
            "Нажмите «Включить звук» у микрофона или клавишу M.",
        )
    } else {
        Check::ok("Микрофон", format!("{} включён", mic.name))
    }];

    // Про сигнал говорим отдельно: включённый микрофон и звучащий микрофон —
    // разные вещи. Отключённый кабель OBS покажет как исправный вход.
    checks.push(match mic.level_db {
        Some(db) if db > SILENT_DB => Check::ok("Сигнал микрофона", format!("Есть, {db:.0} dB")),
        Some(_) if mic.muted => Check::ok("Сигнал микрофона", "Не проверяется: микрофон заглушен"),
        Some(_) => Check::warn(
            "Сигнал микрофона",
            "Тишина в микрофоне",
            "Скажите что-нибудь и проверьте снова. Если тихо — проверьте кабель и выбранное устройство.",
        ),
        None => Check::warn(
            "Сигнал микрофона",
            "Уровень ещё не измерен",
            "Подождите пару секунд и повторите проверку.",
        ),
    });

    checks
}

/// Главная проверка ради незрячего оператора.
///
/// Звук рабочего стола тянет в эфир всё, что звучит на компьютере, включая
/// речь экранного диктора. Зрители слышат, как программа зачитывает
/// интерфейс. Сам OBS такого предупреждения не делает.
fn desktop_audio_check(audio: &[AudioInput]) -> Check {
    let desktop: Vec<&AudioInput> = audio
        .iter()
        .filter(|a| a.role.as_deref() == Some("desktop"))
        .collect();

    // Три разных случая, и путать их нельзя: «нет источника» и «источник
    // заглушен» одинаково безопасны, но чинятся по-разному, а оператор
    // читает эту строку вслух и должен понимать, что у него на самом деле.
    if desktop.is_empty() {
        return Check::ok(
            "Звук рабочего стола",
            "Источник не настроен — звук системы в эфир не идёт",
        );
    }

    let live: Vec<&&AudioInput> = desktop.iter().filter(|a| !a.muted).collect();
    if live.is_empty() {
        return Check::ok(
            "Звук рабочего стола",
            "Заглушен — речь экранного диктора в эфир не попадёт",
        );
    }

    let names: Vec<&str> = live.iter().map(|a| a.name.as_str()).collect();
    Check::warn(
        "Звук рабочего стола",
        format!(
            "Включён ({}). В эфир пойдёт всё, что звучит на компьютере, \
             включая речь экранного диктора",
            names.join(", ")
        ),
        "Если диктор не должен звучать в эфире: заглушите этот источник, \
         а звук игры заведите отдельно через «Захват звука приложения».",
    )
}

fn donation_check(overlay: &OverlayState) -> Check {
    if !overlay.present_in_scenes {
        return Check::warn(
            "DonationAlerts",
            "Оверлей отсутствует в сценах — донаты не покажутся",
            "Нажмите «Проверить и восстановить в OBS».",
        );
    }
    if overlay.muted {
        return Check::warn(
            "DonationAlerts",
            "Оверлей заглушен — донаты не будет слышно",
            "Нажмите «Проверить и восстановить в OBS».",
        );
    }
    if !overlay.on_top {
        return Check::warn(
            "DonationAlerts",
            "Оверлей не верхним слоем — его перекроет игра",
            "Нажмите «Проверить и восстановить в OBS».",
        );
    }
    Check::ok("DonationAlerts", "Оверлей на месте, звук идёт в эфир")
}

fn disk_check(free_mb: f64, recording: bool) -> Check {
    if free_mb <= 0.0 {
        return Check::ok("Свободно на диске", "OBS не сообщил объём");
    }
    let gb = free_mb / 1024.0;
    if free_mb < LOW_DISK_MB {
        let severity_detail = format!("Осталось {gb:.1} ГБ");
        return if recording {
            Check::critical(
                "Свободно на диске",
                format!("{severity_detail} — идёт запись, она может оборваться"),
                "Освободите место или остановите запись.",
            )
        } else {
            Check::warn(
                "Свободно на диске",
                severity_detail,
                "Для долгой записи этого мало.",
            )
        };
    }
    Check::ok("Свободно на диске", format!("{gb:.0} ГБ"))
}

/// Короткая сводка для озвучивания: сколько всего и сколько критичного.
pub fn summary(checks: &[Check]) -> String {
    let critical = checks
        .iter()
        .filter(|c| c.severity == Severity::Critical)
        .count();
    let warnings = checks
        .iter()
        .filter(|c| c.severity == Severity::Warning)
        .count();
    match (critical, warnings) {
        (0, 0) => "Всё готово к эфиру".to_string(),
        (0, w) => format!("Можно начинать, но есть предупреждений: {w}"),
        (c, 0) => format!("Начинать нельзя, критических проблем: {c}"),
        (c, w) => format!("Начинать нельзя, критических проблем: {c}, предупреждений: {w}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic(muted: bool, level: Option<f64>) -> AudioInput {
        AudioInput {
            name: "Микрофон".into(),
            role: Some("mic".into()),
            muted,
            level_db: level,
        }
    }

    fn desktop(muted: bool) -> AudioInput {
        AudioInput {
            name: "Звук раб. стола".into(),
            role: Some("desktop".into()),
            muted,
            level_db: None,
        }
    }

    fn healthy() -> Snapshot {
        Snapshot {
            obs_connected: true,
            obs_version: Some("32.2.1".into()),
            current_scene: Some("Игра".into()),
            sources: vec![SceneSource {
                name: "Захват игры".into(),
                enabled: true,
            }],
            audio: vec![mic(false, Some(-20.0)), desktop(true)],
            free_disk_mb: 80_000.0,
            streaming: false,
            recording: false,
            stream_service_configured: true,
            donation_overlay: Some(OverlayState {
                present_in_scenes: true,
                on_top: true,
                muted: false,
            }),
            twitch_connected: true,
        }
    }

    fn find<'a>(checks: &'a [Check], title: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.title == title)
            .unwrap_or_else(|| panic!("нет проверки «{title}»"))
    }

    #[test]
    fn healthy_setup_has_no_complaints() {
        let checks = evaluate(&healthy());
        assert!(
            checks.iter().all(|c| c.severity == Severity::Ok),
            "неожиданные замечания: {:?}",
            checks
                .iter()
                .filter(|c| c.severity != Severity::Ok)
                .map(|c| &c.title)
                .collect::<Vec<_>>()
        );
        assert_eq!(summary(&checks), "Всё готово к эфиру");
    }

    #[test]
    fn without_obs_we_do_not_guess_the_rest() {
        // Остальные проверки строились бы на пустом снимке и врали бы.
        let checks = evaluate(&Snapshot::default());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].severity, Severity::Critical);
    }

    #[test]
    fn desktop_audio_warns_about_screen_reader_in_the_stream() {
        // Ради этой проверки модуль и написан: OBS о таком не предупреждает,
        // а зритель услышит, как диктор зачитывает оператору интерфейс.
        let mut s = healthy();
        s.audio = vec![mic(false, Some(-20.0)), desktop(false)];
        let checks = evaluate(&s);
        let check = find(&checks, "Звук рабочего стола");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("диктора"), "{}", check.detail);
        assert!(
            check
                .fix
                .as_ref()
                .unwrap()
                .contains("Захват звука приложения")
        );
    }

    #[test]
    fn muted_desktop_audio_is_fine() {
        let checks = evaluate(&healthy());
        let check = find(&checks, "Звук рабочего стола");
        assert_eq!(check.severity, Severity::Ok);
        assert!(check.detail.contains("Заглушен"), "{}", check.detail);
    }

    #[test]
    fn absent_desktop_audio_is_not_called_muted() {
        // Ничего не заглушено — источника просто нет. Случаи одинаково
        // безопасны, но чинятся по-разному, и оператор читает это вслух.
        let mut s = healthy();
        s.audio = vec![mic(false, Some(-20.0))];
        let checks = evaluate(&s);
        let check = find(&checks, "Звук рабочего стола");
        assert_eq!(check.severity, Severity::Ok);
        assert!(check.detail.contains("не настроен"), "{}", check.detail);
    }

    #[test]
    fn muted_microphone_blocks_the_stream() {
        let mut s = healthy();
        s.audio = vec![mic(true, Some(-100.0)), desktop(true)];
        let checks = evaluate(&s);
        assert_eq!(find(&checks, "Микрофон").severity, Severity::Critical);
        // Про сигнал при заглушенном микрофоне ругаться незачем — это шум.
        assert_eq!(find(&checks, "Сигнал микрофона").severity, Severity::Ok);
    }

    #[test]
    fn silent_microphone_is_only_a_warning() {
        // Кабель мог отойти, но эфир при этом технически возможен.
        let mut s = healthy();
        s.audio = vec![mic(false, Some(-100.0)), desktop(true)];
        let checks = evaluate(&s);
        assert_eq!(
            find(&checks, "Сигнал микрофона").severity,
            Severity::Warning
        );
    }

    #[test]
    fn unassigned_microphone_is_critical_and_explains_where_to_fix() {
        let mut s = healthy();
        s.audio = vec![desktop(true)];
        let checks = evaluate(&s);
        let check = find(&checks, "Микрофон");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.fix.as_ref().unwrap().contains("Настройки OBS"));
    }

    #[test]
    fn empty_scene_means_black_screen() {
        let mut s = healthy();
        s.sources = vec![];
        assert_eq!(
            find(&evaluate(&s), "Содержимое сцены").severity,
            Severity::Critical
        );
    }

    #[test]
    fn all_sources_hidden_is_as_bad_as_empty_scene() {
        let mut s = healthy();
        s.sources = vec![
            SceneSource {
                name: "Захват игры".into(),
                enabled: false,
            },
            SceneSource {
                name: "Камера".into(),
                enabled: false,
            },
        ];
        let checks = evaluate(&s);
        let check = find(&checks, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("чёрный экран"));
    }

    #[test]
    fn missing_stream_key_blocks_the_stream() {
        let mut s = healthy();
        s.stream_service_configured = false;
        assert_eq!(
            find(&evaluate(&s), "Сервис вещания").severity,
            Severity::Critical
        );
    }

    #[test]
    fn low_disk_is_critical_only_while_recording() {
        let mut s = healthy();
        s.free_disk_mb = 1000.0;
        assert_eq!(
            find(&evaluate(&s), "Свободно на диске").severity,
            Severity::Warning
        );

        s.recording = true;
        let checks = evaluate(&s);
        let check = find(&checks, "Свободно на диске");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("оборваться"));
    }

    #[test]
    fn donation_overlay_problems_are_reported_separately() {
        let mut s = healthy();
        s.donation_overlay = Some(OverlayState {
            present_in_scenes: true,
            on_top: false,
            muted: false,
        });
        let checks = evaluate(&s);
        let check = find(&checks, "DonationAlerts");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("перекроет"));
    }

    #[test]
    fn donationalerts_absent_is_not_mentioned_at_all() {
        // Не настраивали — значит и проверять нечего, лишний пункт только
        // удлинил бы чтение с экранного диктора.
        let mut s = healthy();
        s.donation_overlay = None;
        assert!(evaluate(&s).iter().all(|c| c.title != "DonationAlerts"));
    }

    #[test]
    fn summary_counts_both_kinds() {
        let mut s = healthy();
        s.audio = vec![mic(true, None), desktop(false)];
        s.stream_service_configured = false;
        let text = summary(&evaluate(&s));
        assert!(text.starts_with("Начинать нельзя"), "{text}");
        assert!(text.contains("критических"), "{text}");
        assert!(text.contains("предупреждений"), "{text}");
    }

    #[test]
    fn warnings_alone_still_allow_streaming() {
        let mut s = healthy();
        s.twitch_connected = false;
        assert!(summary(&evaluate(&s)).starts_with("Можно начинать"));
    }
}
