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
    /// Вид входа OBS, например wasapi_output_capture.
    ///
    /// Роль назначается только слотам desktop1/2 и mic1..4. Обычный
    /// «Захват выходного аудиопотока», добавленный источником в сцену, роли
    /// не получает, но системный звук пишет ровно так же — а значит и речь
    /// экранного диктора. По одной роли такой источник не разглядеть.
    pub kind: Option<String>,
    pub muted: bool,
    /// Обычный audio source реально участвует в текущем program output.
    ///
    /// OBS special desktop1/2 живут глобально и сюда не завязаны. Для
    /// wasapi_output_capture, добавленного как source в старую сцену, это
    /// false — иначе preflight пугал бы диктором там, где source не в эфире.
    pub in_program_output: bool,
    /// Последний измеренный пик, dB. None — измерений ещё не было.
    pub level_db: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct SceneSource {
    pub name: String,
    pub enabled: bool,
    /// Вид источника. None у вложенных сцен и групп.
    pub kind: Option<String>,
    /// Вложенная сцена или группа. Сама по себе больше не считается картинкой:
    /// картинку дают только её раскрытые дети.
    pub is_scene_or_group: bool,
    /// OBS считает source реально активным/показываемым в program output.
    pub active: Verdict,
    /// Раскрытое содержимое вложенной сцены или группы.
    pub children: Vec<SceneSource>,
}

struct VisualSource<'a> {
    name: &'a str,
    enabled: bool,
    active: Verdict,
}

/// Виды входов, которые дают только звук и никакой картинки.
///
/// Всё остальное считаем видимым: список закрытый и короткий, а ошибиться
/// в сторону «это видео» безопаснее — иначе проверка начнёт кричать о
/// чёрном экране там, где его нет, и ей перестанут верить.
const AUDIO_ONLY_KINDS: &[&str] = &[
    "wasapi_input_capture",
    "wasapi_output_capture",
    "wasapi_process_output_capture",
    "coreaudio_input_capture",
    "coreaudio_output_capture",
    "pulse_input_capture",
    "pulse_output_capture",
    "alsa_input_capture",
    "jack_output_capture",
    "sndio_output_capture",
    "audio_line",
];

/// Виды, которые тянут в эфир весь системный звук, а значит и экранный диктор.
///
/// Захват звука отдельного приложения сюда намеренно не входит: он берёт
/// только выбранную программу, и диктор в него не попадает.
const SYSTEM_AUDIO_KINDS: &[&str] = &[
    "wasapi_output_capture",
    "coreaudio_output_capture",
    "pulse_output_capture",
];

impl SceneSource {
    /// Даёт ли leaf-source картинку.
    fn produces_video_leaf(&self) -> bool {
        match self.kind.as_deref() {
            Some(kind) => !AUDIO_ONLY_KINDS.contains(&kind),
            // Вид неизвестен — считаем видимым, чтобы не пугать зря.
            None => true,
        }
    }

    fn collect_visual_sources<'a>(&'a self, parent_enabled: bool, out: &mut Vec<VisualSource<'a>>) {
        let enabled = parent_enabled && self.enabled;
        if self.is_scene_or_group {
            for child in &self.children {
                child.collect_visual_sources(enabled, out);
            }
            return;
        }
        if self.produces_video_leaf() {
            out.push(VisualSource {
                name: &self.name,
                enabled,
                active: self.active,
            });
        }
    }
}

impl AudioInput {
    /// Тянет ли вход весь системный звук.
    fn captures_system_audio(&self) -> bool {
        if self.role.as_deref() == Some("desktop") {
            return true;
        }
        self.kind
            .as_deref()
            .is_some_and(|kind| SYSTEM_AUDIO_KINDS.contains(&kind))
            && self.in_program_output
    }
}

/// Результат проверки, у которого есть третий исход.
///
/// Логическое «да/нет» здесь врёт: запрос к OBS мог не выполниться, и это
/// не то же самое, что «всё хорошо». Прежде такая ошибка молча превращалась
/// в успех, и панель заявляла «оверлей на месте, звук идёт в эфир», ничего
/// на самом деле не проверив.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Проверено, всё в порядке.
    Ok,
    /// Проверено, не в порядке.
    Broken,
    /// Проверить не удалось.
    #[default]
    Unknown,
}

impl Verdict {
    /// Худший из двух исходов при обходе нескольких объектов.
    ///
    /// Сломанное перевешивает неизвестное: про него есть что сказать точно,
    /// и чинить надо в первую очередь его. Неизвестное перевешивает успех,
    /// потому что непроверенное не должно выдаваться за исправное.
    pub fn min_known(self, other: Verdict) -> Verdict {
        match (self, other) {
            (Verdict::Broken, _) | (_, Verdict::Broken) => Verdict::Broken,
            (Verdict::Unknown, _) | (_, Verdict::Unknown) => Verdict::Unknown,
            _ => Verdict::Ok,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OverlayState {
    pub present_in_scenes: Verdict,
    pub on_top: Verdict,
    /// Слышен ли оверлей: Ok — не заглушен.
    pub audible: Verdict,
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
    /// Источник, назначенный камерой. None — роль не назначена, и проверять
    /// нечего: не у каждого эфира есть камера.
    pub camera: Option<String>,
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
    if let Some(camera) = &snapshot.camera {
        checks.push(camera_check(camera, &snapshot.sources));
    }
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

/// Видна ли камера в текущей сцене.
///
/// Проверка существует потому, что оператор не может посмотреть на себя сам.
/// Скрытая камера — не поломка эфира, но почти наверняка не то, что задумано,
/// а узнать об этом иначе можно только от зрителей.
///
/// Ищем по раскрытому дереву: камера может лежать внутри вложенной сцены
/// или группы, и по верхнему уровню её не найти.
fn camera_check(camera: &str, sources: &[SceneSource]) -> Check {
    let mut visual = Vec::new();
    for source in sources {
        source.collect_visual_sources(true, &mut visual);
    }

    match visual.iter().find(|s| s.name == camera) {
        Some(found) if found.enabled => Check::ok("Камера", format!("{camera} видна")),
        Some(_) => Check::warn(
            "Камера",
            format!("{camera} есть в сцене, но скрыта"),
            "Включите её в разделе «Источники», если она должна быть в кадре.",
        ),
        None => Check::warn(
            "Камера",
            format!("{camera} отсутствует в текущей сцене"),
            "Либо добавьте камеру в сцену, либо снимите ей роль в разделе «Роли источников».",
        ),
    }
}

fn scene_content_check(sources: &[SceneSource]) -> Check {
    if sources.is_empty() {
        return Check::critical(
            "Содержимое сцены",
            "В текущей сцене нет источников — зрители увидят чёрный экран",
            "Добавьте источник: захват игры, экрана или камеру.",
        );
    }
    // Считаем только источники с картинкой. Сцена, где включён один
    // микрофон, формально непустая, но в эфир из неё идёт чёрный экран —
    // прежняя проверка на такой конфигурации сообщала «видимых 1 из 1».
    let mut visual = Vec::new();
    for source in sources {
        source.collect_visual_sources(true, &mut visual);
    }
    if visual.is_empty() {
        return Check::critical(
            "Содержимое сцены",
            format!(
                "В сцене только звуковые источники ({}) — зрители увидят чёрный экран",
                sources.len()
            ),
            "Добавьте захват игры, экрана или камеру.",
        );
    }

    let shown_sources: Vec<&VisualSource<'_>> = visual.iter().filter(|s| s.enabled).collect();
    let shown = shown_sources.len();
    if shown == 0 {
        return Check::critical(
            "Содержимое сцены",
            format!(
                "Все источники с картинкой скрыты ({}) — в эфир пойдёт чёрный экран",
                visual.len()
            ),
            "Включите нужный источник в разделе «Источники».",
        );
    }

    let active = shown_sources
        .iter()
        .filter(|s| s.active == Verdict::Ok)
        .count();
    let inactive: Vec<&str> = shown_sources
        .iter()
        .filter(|s| s.active == Verdict::Broken)
        .map(|s| s.name)
        .collect();
    let unknown = shown_sources
        .iter()
        .filter(|s| s.active == Verdict::Unknown)
        .count();

    if active == 0 && !inactive.is_empty() {
        return Check::critical(
            "Содержимое сцены",
            format!(
                "Источники включены, но OBS говорит, что они сейчас не активны: {}",
                inactive.join(", ")
            ),
            "Проверьте окно игры, камеру или захват экрана. Если нужно, выберите другой source.",
        );
    }
    if active == 0 {
        return Check::warn(
            "Содержимое сцены",
            "Источники включены, но OBS не дал проверить active/showing состояние",
            "Повторите проверку. Если есть сомнения — проверьте превью эфира.",
        );
    }
    if !inactive.is_empty() || unknown > 0 {
        let mut parts = Vec::new();
        if !inactive.is_empty() {
            parts.push(format!("не активны: {}", inactive.join(", ")));
        }
        if unknown > 0 {
            parts.push(format!("не проверено: {unknown}"));
        }
        return Check::warn(
            "Содержимое сцены",
            format!(
                "Есть активная картинка: {active} из {shown}; {}",
                parts.join("; ")
            ),
            "Проверьте источники, которые OBS не считает active/showing.",
        );
    }
    Check::ok(
        "Содержимое сцены",
        format!(
            "Активных источников с картинкой: {active} из {}",
            visual.len()
        ),
    )
}

fn microphone_checks(audio: &[AudioInput]) -> Vec<Check> {
    // Роль сюда приходит уже разрешённой: назначение владельца перекрывает
    // роль самого OBS. Поэтому микрофон, заведённый обычным «Захватом входного
    // аудиопотока», больше не даёт «начинать нельзя» — достаточно один раз
    // указать его в разделе «Роли источников».
    let Some(mic) = audio.iter().find(|a| a.role.as_deref() == Some("mic")) else {
        return vec![Check::critical(
            "Микрофон",
            "Не указано, какой источник считать микрофоном",
            "Назначьте его в разделе «Роли источников» либо у актёра в OBS: \
             Настройки → Аудио → «Микрофон/дополнительное аудио». \
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
    let desktop: Vec<&AudioInput> = audio.iter().filter(|a| a.captures_system_audio()).collect();

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
    const FIX: &str = "Нажмите «Проверить и восстановить в OBS».";

    // Сломанное важнее неизвестного: о нём есть что сказать точно.
    if overlay.present_in_scenes == Verdict::Broken {
        return Check::warn(
            "DonationAlerts",
            "Оверлей отсутствует в сценах — донаты не покажутся",
            FIX,
        );
    }
    if overlay.audible == Verdict::Broken {
        return Check::warn(
            "DonationAlerts",
            "Оверлей заглушен — донаты не будет слышно",
            FIX,
        );
    }
    if overlay.on_top == Verdict::Broken {
        return Check::warn(
            "DonationAlerts",
            "Оверлей не верхним слоем — его перекроет игра",
            FIX,
        );
    }

    // Непроверенное не выдаём за исправное: молчание тут означало бы, что
    // оператор считает донаты работающими, ничего о них не зная.
    let unknown = [
        ("наличие в сценах", overlay.present_in_scenes),
        ("звук", overlay.audible),
        ("порядок слоёв", overlay.on_top),
    ]
    .into_iter()
    .filter(|(_, v)| *v == Verdict::Unknown)
    .map(|(name, _)| name)
    .collect::<Vec<_>>();

    if !unknown.is_empty() {
        return Check::warn(
            "DonationAlerts",
            format!("Не удалось проверить: {}", unknown.join(", ")),
            "OBS не ответил на часть запросов. Повторите проверку.",
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
            kind: Some("wasapi_input_capture".into()),
            muted,
            in_program_output: true,
            level_db: level,
        }
    }

    fn desktop(muted: bool) -> AudioInput {
        AudioInput {
            name: "Звук раб. стола".into(),
            role: Some("desktop".into()),
            kind: Some("wasapi_output_capture".into()),
            muted,
            in_program_output: false,
            level_db: None,
        }
    }

    fn visual(name: &str, enabled: bool, active: Verdict) -> SceneSource {
        SceneSource {
            name: name.into(),
            enabled,
            kind: Some("game_capture".into()),
            is_scene_or_group: false,
            active,
            children: Vec::new(),
        }
    }

    fn audio_source(name: &str, kind: &str) -> SceneSource {
        SceneSource {
            name: name.into(),
            enabled: true,
            kind: Some(kind.into()),
            is_scene_or_group: false,
            active: Verdict::Unknown,
            children: Vec::new(),
        }
    }

    fn camera_source(name: &str, enabled: bool) -> SceneSource {
        SceneSource {
            name: name.into(),
            enabled,
            kind: Some("dshow_input".into()),
            is_scene_or_group: false,
            active: Verdict::Ok,
            children: Vec::new(),
        }
    }

    fn nested(name: &str, enabled: bool, children: Vec<SceneSource>) -> SceneSource {
        SceneSource {
            name: name.into(),
            enabled,
            kind: None,
            is_scene_or_group: true,
            active: Verdict::Unknown,
            children,
        }
    }

    fn healthy() -> Snapshot {
        Snapshot {
            obs_connected: true,
            obs_version: Some("32.2.1".into()),
            current_scene: Some("Игра".into()),
            sources: vec![visual("Захват игры", true, Verdict::Ok)],
            audio: vec![mic(false, Some(-20.0)), desktop(true)],
            free_disk_mb: 80_000.0,
            streaming: false,
            recording: false,
            stream_service_configured: true,
            donation_overlay: Some(OverlayState {
                present_in_scenes: Verdict::Ok,
                on_top: Verdict::Ok,
                audible: Verdict::Ok,
            }),
            twitch_connected: true,
            camera: None,
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
    fn camera_is_not_checked_until_a_role_is_assigned() {
        // Не у каждого эфира есть камера. Лишний пункт удлиняет чтение
        // с экранного диктора и ничего не сообщает.
        let s = healthy();
        assert!(s.camera.is_none());
        assert!(evaluate(&s).iter().all(|c| c.title != "Камера"));
    }

    #[test]
    fn hidden_camera_is_reported() {
        // Посмотреть на себя оператор не может, а скрытая камера почти
        // наверняка не то, что задумано.
        let mut s = healthy();
        s.camera = Some("Sony".into());
        s.sources.push(camera_source("Sony", false));
        let checks = evaluate(&s);
        let check = find(&checks, "Камера");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("скрыта"), "{}", check.detail);
    }

    #[test]
    fn camera_missing_from_scene_is_reported() {
        let mut s = healthy();
        s.camera = Some("Sony".into());
        let checks = evaluate(&s);
        let check = find(&checks, "Камера");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("отсутствует"), "{}", check.detail);
    }

    #[test]
    fn camera_inside_a_nested_scene_is_found() {
        // Камеру часто прячут внутрь группы: по верхнему уровню её не видно,
        // и без обхода дерева проверка соврала бы «отсутствует».
        let mut s = healthy();
        s.camera = Some("Sony".into());
        s.sources.push(nested(
            "Группа камеры",
            true,
            vec![camera_source("Sony", true)],
        ));
        assert_eq!(find(&evaluate(&s), "Камера").severity, Severity::Ok);
    }

    #[test]
    fn unassigned_microphone_is_critical_and_explains_where_to_fix() {
        let mut s = healthy();
        s.audio = vec![desktop(true)];
        let checks = evaluate(&s);
        let check = find(&checks, "Микрофон");
        assert_eq!(check.severity, Severity::Critical);
        // Способов исправить два, и назвать надо оба: роль можно указать в
        // панели, а можно назначить слот в самом OBS.
        let fix = check.fix.as_ref().unwrap();
        assert!(fix.contains("Роли источников"), "{fix}");
        assert!(fix.contains("Аудио"), "{fix}");
    }

    #[test]
    fn scene_with_only_audio_sources_is_a_black_screen() {
        // Самый коварный случай: сцена «непустая», всё включено, а зрители
        // смотрят в черноту. Прежняя проверка отвечала «видимых 1 из 1».
        let mut s = healthy();
        s.sources = vec![
            audio_source("Микрофон", "wasapi_input_capture"),
            audio_source("Звук игры", "wasapi_process_output_capture"),
        ];
        let checks = evaluate(&s);
        let check = find(&checks, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("чёрный экран"), "{}", check.detail);
        assert!(summary(&checks).starts_with("Начинать нельзя"));
    }

    #[test]
    fn audio_sources_do_not_pad_the_visible_count() {
        // Один захват игры плюс два звуковых входа — это «1 из 1», а не «3 из 3».
        let mut s = healthy();
        s.sources = vec![
            visual("Захват игры", true, Verdict::Ok),
            audio_source("Микрофон", "wasapi_input_capture"),
            audio_source("Звук приложения", "wasapi_process_output_capture"),
        ];
        let checks = evaluate(&s);
        let check = find(&checks, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Ok);
        assert!(check.detail.contains("1 из 1"), "{}", check.detail);
    }

    #[test]
    fn nested_scene_counts_its_real_picture_children() {
        let mut s = healthy();
        s.sources = vec![nested(
            "Игра во вложенной сцене",
            true,
            vec![visual("Захват игры", true, Verdict::Ok)],
        )];
        assert_eq!(
            find(&evaluate(&s), "Содержимое сцены").severity,
            Severity::Ok
        );
    }

    #[test]
    fn empty_nested_scene_is_not_a_picture() {
        let mut s = healthy();
        s.sources = vec![nested("Пустая вложенная сцена", true, vec![])];
        let checks = evaluate(&s);
        let check = find(&checks, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("чёрный экран"), "{}", check.detail);
    }

    #[test]
    fn group_with_only_audio_children_is_a_black_screen() {
        let mut s = healthy();
        s.sources = vec![nested(
            "Группа звука",
            true,
            vec![audio_source("Микрофон", "wasapi_input_capture")],
        )];
        let check_owner = evaluate(&s);
        let check = find(&check_owner, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("чёрный экран"), "{}", check.detail);
    }

    #[test]
    fn enabled_visual_source_must_be_active_or_showing() {
        let mut s = healthy();
        s.sources = vec![visual("Захват игры", true, Verdict::Broken)];
        let check_owner = evaluate(&s);
        let check = find(&check_owner, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("не активны"), "{}", check.detail);
    }

    #[test]
    fn unknown_active_state_is_not_reported_as_green() {
        let mut s = healthy();
        s.sources = vec![visual("Захват игры", true, Verdict::Unknown)];
        let check_owner = evaluate(&s);
        let check = find(&check_owner, "Содержимое сцены");
        assert_eq!(check.severity, Severity::Warning);
        assert!(
            check.detail.contains("не дал проверить"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn system_audio_capture_without_role_still_warns() {
        // Обычный «Захват выходного аудиопотока» роли desktop не получает,
        // но пишет весь системный звук — вместе с речью экранного диктора.
        // Прежняя проверка отвечала «источник не настроен» и успокаивала.
        let mut s = healthy();
        s.audio = vec![
            mic(false, Some(-20.0)),
            AudioInput {
                name: "Захват выходного аудиопотока".into(),
                role: None,
                kind: Some("wasapi_output_capture".into()),
                muted: false,
                in_program_output: true,
                level_db: None,
            },
        ];
        let check_owner = evaluate(&s);
        let check = find(&check_owner, "Звук рабочего стола");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("диктора"), "{}", check.detail);
    }

    #[test]
    fn system_audio_capture_outside_program_output_does_not_warn() {
        let mut s = healthy();
        s.audio = vec![
            mic(false, Some(-20.0)),
            AudioInput {
                name: "Старый системный звук".into(),
                role: None,
                kind: Some("wasapi_output_capture".into()),
                muted: false,
                in_program_output: false,
                level_db: None,
            },
        ];
        let check_owner = evaluate(&s);
        let check = find(&check_owner, "Звук рабочего стола");
        assert_eq!(check.severity, Severity::Ok);
        assert!(check.detail.contains("не настроен"), "{}", check.detail);
    }

    #[test]
    fn application_audio_capture_is_not_a_screen_reader_risk() {
        // Захват звука отдельного приложения берёт только его, диктор туда
        // не попадает. Ругаться на него — плодить ложные тревоги.
        let mut s = healthy();
        s.audio = vec![
            mic(false, Some(-20.0)),
            AudioInput {
                name: "Звук игры".into(),
                role: None,
                kind: Some("wasapi_process_output_capture".into()),
                muted: false,
                in_program_output: true,
                level_db: None,
            },
        ];
        assert_eq!(
            find(&evaluate(&s), "Звук рабочего стола").severity,
            Severity::Ok
        );
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
            visual("Захват игры", false, Verdict::Broken),
            visual("Камера", false, Verdict::Broken),
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
            present_in_scenes: Verdict::Ok,
            on_top: Verdict::Broken,
            audible: Verdict::Ok,
        });
        let checks = evaluate(&s);
        let check = find(&checks, "DonationAlerts");
        assert_eq!(check.severity, Severity::Warning);
        assert!(check.detail.contains("перекроет"));
    }

    #[test]
    fn unverified_overlay_is_not_reported_as_working() {
        // OBS не ответил на часть запросов. Прежде это давало «оверлей на
        // месте, звук идёт в эфир»: панель уверяла, что донаты работают,
        // не проверив о них ровно ничего.
        let mut s = healthy();
        s.donation_overlay = Some(OverlayState {
            present_in_scenes: Verdict::Unknown,
            on_top: Verdict::Unknown,
            audible: Verdict::Ok,
        });
        let checks = evaluate(&s);
        let check = find(&checks, "DonationAlerts");
        assert_eq!(check.severity, Severity::Warning);
        assert!(
            check.detail.contains("Не удалось проверить"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn broken_overlay_outweighs_unverified_parts() {
        // Про сломанное есть что сказать точно — его и показываем первым.
        let mut s = healthy();
        s.donation_overlay = Some(OverlayState {
            present_in_scenes: Verdict::Unknown,
            on_top: Verdict::Broken,
            audible: Verdict::Unknown,
        });
        let checks = evaluate(&s);
        assert!(find(&checks, "DonationAlerts").detail.contains("перекроет"));
    }

    #[test]
    fn verdict_combination_never_upgrades_to_ok() {
        use Verdict::*;
        assert_eq!(Ok.min_known(Unknown), Unknown);
        assert_eq!(Unknown.min_known(Ok), Unknown);
        assert_eq!(Unknown.min_known(Broken), Broken);
        assert_eq!(Broken.min_known(Ok), Broken);
        assert_eq!(Ok.min_known(Ok), Ok);
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
