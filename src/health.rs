//! Наблюдение за здоровьем эфира.
//!
//! Владелец панели не видит экран актёра и не слышит его звук. Поломки,
//! очевидные для зрителей — заикания картинки, оборвавшаяся запись — для него
//! невидимы. Этот модуль превращает счётчики OBS в тревоги.
//!
//! Логика намеренно отделена от ввода-вывода: она чистая и проверяется
//! тестами без запущенного OBS.

use serde::Serialize;
use std::time::Duration;

/// Как часто снимаем показания. Реже — тревога опаздывает, чаще — шум
/// в статистике начинает перевешивать сигнал.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Порог срабатывания по потерянным кадрам, % за окно.
const FRAMES_WARN_PERCENT: f64 = 1.0;
/// Порог отбоя. Разведён с порогом срабатывания намеренно: на одном пороге
/// тревога дребезжала бы при колебании вокруг границы.
const FRAMES_CLEAR_PERCENT: f64 = 0.2;

/// Порог по свободному месту, МБ. Ниже этого запись рискует оборваться.
const DISK_WARN_MB: f64 = 2048.0;
/// Отбой по месту — тоже с запасом, чтобы не мигало.
const DISK_CLEAR_MB: f64 = 4096.0;

/// Показания OBS за один замер.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    /// Всего кадров, выданных выводом с начала эфира.
    pub total_frames: f64,
    /// Из них потеряно.
    pub skipped_frames: f64,
    /// Свободно на диске, МБ.
    pub free_disk_mb: f64,
    /// Идёт ли эфир прямо сейчас.
    pub streaming: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Alert {
    /// Кадры теряются прямо сейчас.
    FramesDropping { percent: f64 },
    /// Потери прекратились.
    FramesRecovered,
    /// Свободного места мало.
    DiskLow { free_mb: f64 },
    /// Места снова достаточно.
    DiskRecovered,
}

impl Alert {
    /// Текст для панели и лога. Держим рядом с вариантом, чтобы формулировка
    /// не разъезжалась между местами использования.
    pub fn message(&self) -> String {
        match self {
            Alert::FramesDropping { percent } => format!(
                "Эфир теряет кадры: {percent:.1} % за последние {} секунд. \
                 Обычно это нехватка интернета или процессора у актёра.",
                SAMPLE_INTERVAL.as_secs()
            ),
            Alert::FramesRecovered => "Потеря кадров прекратилась".to_string(),
            Alert::DiskLow { free_mb } => format!(
                "На диске у актёра осталось {:.1} ГБ. Запись может оборваться.",
                free_mb / 1024.0
            ),
            Alert::DiskRecovered => "Места на диске снова достаточно".to_string(),
        }
    }

    /// Требует ли тревога немедленного внимания. Влияет на то, перебьёт ли
    /// сообщение текущую речь скринридера.
    pub fn is_urgent(&self) -> bool {
        matches!(self, Alert::FramesDropping { .. } | Alert::DiskLow { .. })
    }
}

/// Состояние наблюдателя между замерами.
#[derive(Debug, Default)]
pub struct HealthWatch {
    previous: Option<Sample>,
    frames_alerted: bool,
    disk_alerted: bool,
}

impl HealthWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Принимает очередной замер и возвращает тревоги, о которых стоит сказать.
    ///
    /// Считаем приращение с прошлого замера, а не долю с начала эфира.
    /// Накопительный процент для тревог не годится: неудачная минута в начале
    /// трёхчасового стрима держала бы тревогу включённой навсегда, а свежая
    /// вспышка потерь растворялась бы в большой сумме.
    pub fn observe(&mut self, sample: Sample) -> Vec<Alert> {
        let mut alerts = Vec::new();
        let previous = self.previous.replace(sample);

        // Диск проверяем всегда: запись может идти и без эфира.
        if sample.free_disk_mb > 0.0 {
            if !self.disk_alerted && sample.free_disk_mb < DISK_WARN_MB {
                self.disk_alerted = true;
                alerts.push(Alert::DiskLow {
                    free_mb: sample.free_disk_mb,
                });
            } else if self.disk_alerted && sample.free_disk_mb > DISK_CLEAR_MB {
                self.disk_alerted = false;
                alerts.push(Alert::DiskRecovered);
            }
        }

        // Кадры имеют смысл только пока идёт эфир.
        if !sample.streaming {
            self.frames_alerted = false;
            return alerts;
        }
        let Some(previous) = previous.filter(|p| p.streaming) else {
            // Первый замер после старта эфира: сравнивать не с чем.
            return alerts;
        };

        let frames = sample.total_frames - previous.total_frames;
        let skipped = sample.skipped_frames - previous.skipped_frames;
        // Счётчики обнуляются при перезапуске эфира и тогда уходят в минус.
        if frames <= 0.0 || skipped < 0.0 {
            return alerts;
        }

        let percent = (skipped / frames) * 100.0;
        if !self.frames_alerted && percent >= FRAMES_WARN_PERCENT {
            self.frames_alerted = true;
            alerts.push(Alert::FramesDropping { percent });
        } else if self.frames_alerted && percent <= FRAMES_CLEAR_PERCENT {
            self.frames_alerted = false;
            alerts.push(Alert::FramesRecovered);
        }
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streaming(total: f64, skipped: f64) -> Sample {
        Sample {
            total_frames: total,
            skipped_frames: skipped,
            free_disk_mb: 100_000.0,
            streaming: true,
        }
    }

    #[test]
    fn first_sample_cannot_alert() {
        let mut watch = HealthWatch::new();
        // Сравнивать не с чем, даже если счётчик потерь огромен.
        assert!(watch.observe(streaming(1000.0, 500.0)).is_empty());
    }

    #[test]
    fn steady_stream_stays_quiet() {
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        assert!(watch.observe(streaming(1600.0, 0.0)).is_empty());
    }

    #[test]
    fn fresh_burst_of_drops_raises_alert() {
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        // 30 потерянных из 600 новых — 5 %.
        let alerts = watch.observe(streaming(1600.0, 30.0));
        assert_eq!(alerts.len(), 1);
        match &alerts[0] {
            Alert::FramesDropping { percent } => assert!((percent - 5.0).abs() < 0.01),
            other => panic!("ожидалась тревога о кадрах, получено {other:?}"),
        }
    }

    #[test]
    fn old_drops_do_not_keep_alerting() {
        // Главная причина считать приращение, а не долю с начала эфира:
        // после плохой минуты стрим идёт чисто, и тревога обязана умолкнуть.
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        watch.observe(streaming(1600.0, 60.0)); // 10 % — тревога
        assert!(watch.frames_alerted);

        let alerts = watch.observe(streaming(2200.0, 60.0)); // новых потерь нет
        assert_eq!(alerts, vec![Alert::FramesRecovered]);
        assert!(!watch.frames_alerted);
    }

    #[test]
    fn alert_fires_once_while_trouble_continues() {
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        assert_eq!(watch.observe(streaming(1600.0, 60.0)).len(), 1);
        // Потери продолжаются — повторно не кричим.
        assert!(watch.observe(streaming(2200.0, 120.0)).is_empty());
        assert!(watch.observe(streaming(2800.0, 180.0)).is_empty());
    }

    #[test]
    fn hysteresis_prevents_flapping_at_the_threshold() {
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        watch.observe(streaming(2000.0, 15.0)); // 1.5 % — тревога
        assert!(watch.frames_alerted);
        // 0.5 % — уже ниже порога срабатывания, но выше порога отбоя.
        // На одном пороге здесь был бы отбой, а через замер снова тревога.
        assert!(watch.observe(streaming(3000.0, 20.0)).is_empty());
        assert!(watch.frames_alerted);
    }

    #[test]
    fn restarted_stream_resets_counters_without_false_alert() {
        // OBS обнуляет счётчики при перезапуске эфира: приращение уходит
        // в минус, и без защиты это выглядело бы как деление на мусор.
        let mut watch = HealthWatch::new();
        watch.observe(streaming(100_000.0, 500.0));
        assert!(watch.observe(streaming(10.0, 0.0)).is_empty());
    }

    #[test]
    fn stopping_stream_clears_frame_alert() {
        let mut watch = HealthWatch::new();
        watch.observe(streaming(1000.0, 0.0));
        watch.observe(streaming(1600.0, 60.0));
        assert!(watch.frames_alerted);

        let mut stopped = streaming(1600.0, 60.0);
        stopped.streaming = false;
        watch.observe(stopped);
        assert!(!watch.frames_alerted, "после остановки эфира тревога снята");
    }

    #[test]
    fn low_disk_alerts_once_and_recovers_with_margin() {
        let mut watch = HealthWatch::new();
        let sample = |free| Sample {
            free_disk_mb: free,
            ..Default::default()
        };
        assert_eq!(
            watch.observe(sample(1000.0)),
            vec![Alert::DiskLow { free_mb: 1000.0 }]
        );
        // Всё ещё мало — молчим.
        assert!(watch.observe(sample(900.0)).is_empty());
        // Между порогами: освободилось, но не настолько, чтобы трубить отбой.
        assert!(watch.observe(sample(3000.0)).is_empty());
        assert_eq!(watch.observe(sample(5000.0)), vec![Alert::DiskRecovered]);
    }

    #[test]
    fn missing_disk_reading_is_ignored() {
        // OBS иногда отдаёт ноль вместо размера. Это не «диск переполнен».
        let mut watch = HealthWatch::new();
        assert!(watch.observe(Sample::default()).is_empty());
    }

    #[test]
    fn messages_name_the_cause_not_just_the_symptom() {
        let dropping = Alert::FramesDropping { percent: 3.5 }.message();
        assert!(dropping.contains("3.5"));
        assert!(dropping.contains("интернета") || dropping.contains("процессора"));

        let disk = Alert::DiskLow { free_mb: 1536.0 }.message();
        assert!(
            disk.contains("1.5"),
            "гигабайты читаются легче мегабайт: {disk}"
        );
    }

    #[test]
    fn only_problems_are_urgent() {
        assert!(Alert::FramesDropping { percent: 2.0 }.is_urgent());
        assert!(Alert::DiskLow { free_mb: 100.0 }.is_urgent());
        assert!(!Alert::FramesRecovered.is_urgent());
        assert!(!Alert::DiskRecovered.is_urgent());
    }
}
