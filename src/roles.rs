//! Назначение ролей источникам OBS.
//!
//! OBS знает роли только для своих слотов: «Микрофон/дополнительное аудио» и
//! «Звук рабочего стола». Обычный «Захват входного аудиопотока», добавленный
//! источником в сцену, прекрасно работает микрофоном, но роли не имеет — и
//! проверка готовности заявляла «OBS не знает, какой источник считать
//! микрофоном, начинать нельзя», хотя всё было настроено верно.
//!
//! Здесь владелец назначает роли сам, один раз. Порядок разрешения жёсткий:
//!
//! 1. явное назначение владельца;
//! 2. роль, назначенная самим OBS;
//! 3. ничего — и тогда честный отказ.
//!
//! Третьего пункта «угадать по названию» нет намеренно: именно угадывание
//! приводило к тому, что аварийная клавиша заглушения могла выключить звук
//! игры вместо микрофона, а незрячий оператор этого не замечал.

use serde::{Deserialize, Serialize};

/// Роли, у которых есть потребитель в коде.
///
/// Новые добавляются только вместе с логикой, которая их использует:
/// настройка, которую никто не читает, лишь сбивает с толку.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceRoles {
    /// Основной микрофон. Им управляет горячая клавиша заглушения, по нему
    /// же работает тревога о тишине и проверка готовности.
    pub microphone: Option<String>,
    /// Камера. Проверка готовности следит, что она видна в текущей сцене:
    /// оператор не может посмотреть на себя сам.
    pub camera: Option<String>,
}

impl SourceRoles {
    pub fn from_json(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Пустые строки означают «не назначено»: из формы приходит именно она,
    /// когда владелец сбрасывает выбор.
    pub fn normalized(mut self) -> Self {
        self.microphone = normalize(self.microphone);
        self.camera = normalize(self.camera);
        self
    }

    /// Имя микрофона с учётом порядка разрешения.
    ///
    /// `obs_special` — источник, которому роль микрофона присвоил сам OBS.
    pub fn microphone_name<'a>(&'a self, obs_special: Option<&'a str>) -> Option<&'a str> {
        self.microphone.as_deref().or(obs_special)
    }

    /// Откуда взялось имя микрофона. Нужно панели, чтобы объяснить владельцу,
    /// почему выбран именно этот источник.
    pub fn microphone_origin(&self, obs_special: Option<&str>) -> RoleOrigin {
        match (self.microphone.as_deref(), obs_special) {
            (Some(_), _) => RoleOrigin::Assigned,
            (None, Some(_)) => RoleOrigin::ObsSpecial,
            (None, None) => RoleOrigin::Missing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleOrigin {
    /// Назначено владельцем в панели.
    Assigned,
    /// Взято из настроек самого OBS.
    ObsSpecial,
    /// Не назначено нигде.
    Missing,
}

fn normalize(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_wins_over_obs_role() {
        // У актёра микрофон заведён обычным источником в сцене, а слот OBS
        // занят чем-то другим. Слушать надо владельца.
        let roles = SourceRoles {
            microphone: Some("Focusrite USB".into()),
            camera: None,
        };
        assert_eq!(
            roles.microphone_name(Some("Микр/доп")),
            Some("Focusrite USB")
        );
        assert_eq!(
            roles.microphone_origin(Some("Микр/доп")),
            RoleOrigin::Assigned
        );
    }

    #[test]
    fn obs_role_is_used_when_nothing_assigned() {
        let roles = SourceRoles::default();
        assert_eq!(roles.microphone_name(Some("Микр/доп")), Some("Микр/доп"));
        assert_eq!(
            roles.microphone_origin(Some("Микр/доп")),
            RoleOrigin::ObsSpecial
        );
    }

    #[test]
    fn nothing_assigned_and_no_obs_role_means_missing() {
        // Именно здесь прежний код начинал угадывать и мог заглушить игру.
        let roles = SourceRoles::default();
        assert_eq!(roles.microphone_name(None), None);
        assert_eq!(roles.microphone_origin(None), RoleOrigin::Missing);
    }

    #[test]
    fn blank_assignment_is_treated_as_absent() {
        // Форма присылает пустую строку, когда владелец сбрасывает выбор.
        let roles = SourceRoles {
            microphone: Some("   ".into()),
            camera: Some("".into()),
        }
        .normalized();
        assert_eq!(roles.microphone, None);
        assert_eq!(roles.camera, None);
        assert_eq!(roles.microphone_name(Some("Микр/доп")), Some("Микр/доп"));
    }

    #[test]
    fn assignment_survives_a_restart() {
        let roles = SourceRoles {
            microphone: Some("Focusrite USB".into()),
            camera: Some("Sony".into()),
        };
        assert_eq!(SourceRoles::from_json(&roles.to_json()), roles);
    }

    #[test]
    fn broken_file_does_not_break_startup() {
        // Повреждённый файл ролей не должен мешать запуску: без него всё
        // просто откатывается на роли самого OBS.
        assert_eq!(SourceRoles::from_json("не json"), SourceRoles::default());
        assert_eq!(SourceRoles::from_json(""), SourceRoles::default());
    }

    #[test]
    fn partial_file_keeps_the_rest_empty() {
        let roles = SourceRoles::from_json(r#"{"microphone":"Петличка"}"#);
        assert_eq!(roles.microphone.as_deref(), Some("Петличка"));
        assert_eq!(roles.camera, None);
    }
}
