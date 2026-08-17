//! DonationAlerts: настройка виджета в OBS, лента донатов, OAuth.
//!
//! Здесь же живёт reconcile — приведение OBS к состоянию, в котором донаты
//! видны и слышны в эфире, и проверка того, что обещанное действительно
//! выполнено. Вынесено из main.rs, где всё это соседствовало с маршрутами,
//! авторизацией и OBS.

use super::*;

pub(crate) async fn da_status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(donationalerts_status_value(&st).await))
}

pub(crate) async fn da_config_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let c = effective_donationalerts_config(&st);
    Ok(Json(json!({
        "client_id": c.client_id,
        "client_secret_configured": !c.client_secret.is_empty(),
        "redirect_uri": c.redirect_uri,
        "oauth_enabled": c.oauth_enabled,
        "oauth_scopes": c.oauth_scopes,
    })))
}

pub(crate) async fn da_config_set(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let current = effective_donationalerts_config(&st);
    let client_id = required_trimmed(&body, "clientId", "DonationAlerts client_id обязателен")
        .map_err(|e| bad(&e))?
        .to_string();
    let redirect_uri = required_trimmed(
        &body,
        "redirectUri",
        "DonationAlerts redirect URI обязателен",
    )
    .map_err(|e| bad(&e))?
    .to_string();
    let scopes = scopes_from_body(&body, "scopes", &current.oauth_scopes);
    let secret = body
        .get("clientSecret")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if secret.is_none() && current.client_secret.is_empty() {
        return Err(bad(
            "DonationAlerts client_secret обязателен при первой настройке.",
        ));
    }

    save_secret("donationalerts_client_id", &client_id)
        .map_err(|e| err("Не удалось сохранить DonationAlerts client_id", e))?;
    save_secret("donationalerts_redirect_uri", &redirect_uri)
        .map_err(|e| err("Не удалось сохранить DonationAlerts redirect URI", e))?;
    save_secret(
        "donationalerts_oauth_scopes",
        &serde_json::to_string(&scopes).unwrap_or_default(),
    )
    .map_err(|e| err("Не удалось сохранить DonationAlerts scopes", e))?;
    if let Some(secret) = &secret {
        save_secret("donationalerts_client_secret", secret)
            .map_err(|e| err("Не удалось сохранить DonationAlerts client_secret", e))?;
    }
    save_host_config_update(|cfg| {
        cfg.donationalerts.client_id = client_id.clone();
        cfg.donationalerts.client_secret.clear();
        cfg.donationalerts.redirect_uri = redirect_uri.clone();
        cfg.donationalerts.oauth_scopes = scopes.clone();
        cfg.donationalerts.oauth_enabled = true;
    })
    .map_err(|e| err("Не удалось обновить config/host.json", e))?;

    Ok(Json(json!({
        "ok": true,
        "client_id": client_id,
        "client_secret_configured": secret.is_some() || !current.client_secret.is_empty(),
        "redirect_uri": redirect_uri,
        "oauth_scopes": scopes,
    })))
}

pub(crate) async fn donationalerts_status_value(st: &AppState) -> Value {
    let c = effective_donationalerts_config(st);
    let widget_mute = st
        .obs
        .request("GetInputMute", json!({"inputName": c.input_name}))
        .await
        .ok()
        .and_then(|v| v.get("inputMuted").cloned());
    let widget_volume_db = st
        .obs
        .request("GetInputVolume", json!({"inputName": c.input_name}))
        .await
        .ok()
        .and_then(|v| {
            v.get("inputVolumeMul")
                .and_then(Value::as_f64)
                .map(mul_to_db)
                .map(|db| json!(db))
        });
    // Слышит ли донаты сам владелец. Спрашиваем OBS, а не берём из настройки:
    // настройку могли поменять после последнего применения, и тогда панель
    // сообщала бы желаемое вместо действительного.
    let heard_by_owner = st
        .obs
        .request(
            "GetInputAudioMonitorType",
            json!({"inputName": c.input_name}),
        )
        .await
        .ok()
        .and_then(|v| {
            v.get("monitorType")
                .and_then(Value::as_str)
                .map(|t| t.contains("MONITOR"))
        });
    json!({
        "enabled": c.enabled,
        // Исход последней попытки подключения. Без него панель могла сообщить
        // только «OAuth не пройден», не сказав почему.
        "oauth_last_result": st.donationalerts_oauth_note.read().await.clone(),
        // Незрячий владелец не видит алерт на экране, и звук — единственный
        // способ узнать о донате. Он обязан видеть, включён этот звук или нет.
        "heard_by_owner": heard_by_owner,
        "widget_url_configured": load_secret("donationalerts_widget_url").ok().flatten().is_some(),
        "oauth_configured": !c.client_id.is_empty() && !c.client_secret.is_empty(),
        "tokens_stored": load_secret("donationalerts_tokens").ok().flatten().is_some(),
        "realtime": st.feed.status().await,
        "overlay_scene": c.overlay_scene_name,
        "input_name": c.input_name,
        "widget_muted": widget_mute,
        "widget_volume_db": widget_volume_db,
    })
}

pub(crate) async fn da_recent(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    Ok(Json(json!({"donations": st.feed.recent().await})))
}

pub(crate) async fn da_widget_url(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let url = body.get("url").and_then(Value::as_str).unwrap_or("").trim();
    // Виджет DonationAlerts всегда отдаётся по https; http означает опечатку
    // или подмену, и в OBS такой источник грузить не стоит.
    if !url.starts_with("https://") {
        return Err(bad(
            "Введите https-ссылку Alerts Widget со страницы DonationAlerts.",
        ));
    }
    save_secret("donationalerts_widget_url", url)
        .map_err(|e| err("Не удалось сохранить DonationAlerts URL", e))?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("URL сохранён, но настроить OBS не удалось", e))
}

pub(crate) async fn da_reconcile(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    reconcile_donationalerts(&st)
        .await
        .map(Json)
        .map_err(|e| err("Не удалось настроить DonationAlerts в OBS", e))
}

pub(crate) async fn da_widget_refresh(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let name = &st.cfg.donationalerts.input_name;
    let pressed = st
        .obs
        .request(
            "PressInputPropertiesButton",
            json!({"inputName": name, "propertyName": "refreshnocache"}),
        )
        .await;
    // Старые сборки OBS не знают эту кнопку — тогда достаточно переприменить настройки.
    let result = match pressed {
        Ok(v) => Ok(v),
        Err(_) => {
            st.obs
                .request(
                    "SetInputSettings",
                    json!({"inputName": name, "inputSettings": {}, "overlay": true}),
                )
                .await
        }
    };
    result
        .map(Json)
        .map_err(|e| err("Не удалось обновить виджет", e))
}

pub(crate) async fn da_widget_mute(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let muted = body
        .get("muted")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad("muted обязателен"))?;
    st.obs
        .request(
            "SetInputMute",
            json!({"inputName": st.cfg.donationalerts.input_name, "inputMuted": muted}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить mute DonationAlerts", e))
}

pub(crate) async fn da_widget_volume(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let db = body
        .get("volumeDb")
        .and_then(Value::as_f64)
        .ok_or_else(|| bad("volumeDb обязателен"))?;
    st.obs
        .request(
            "SetInputVolume",
            json!({"inputName": st.cfg.donationalerts.input_name, "inputVolumeMul": db_to_mul(db)}),
        )
        .await
        .map(Json)
        .map_err(|e| err("Не удалось изменить громкость DonationAlerts", e))
}

/// Как OBS должен выводить звук алертов, по настройке `monitoring`.
///
/// «Слышит актёр» и «слышат зрители» — независимые вещи, и OBS различает три
/// сочетания. Понимаем и человеческие написания, и сами имена OBS: значение
/// правят руками в host.json, и спотыкаться об регистр или синоним не должно.
///
/// Неизвестное значение трактуем как «слышно всем»: молча оставить незрячего
/// владельца без звука — худший из возможных исходов опечатки.
pub(crate) fn monitor_type(setting: &str) -> &'static str {
    match setting.trim().to_ascii_lowercase().as_str() {
        // Только в эфир: актёр не слышит ничего.
        "off" | "none" | "output" | "obs_monitoring_type_none" => "OBS_MONITORING_TYPE_NONE",
        // Только актёру, мимо эфира. Нужно при отладке звука.
        "monitor" | "monitor_only" | "obs_monitoring_type_monitor_only" => {
            "OBS_MONITORING_TYPE_MONITOR_ONLY"
        }
        _ => "OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT",
    }
}

/// Приводит OBS к состоянию, в котором донаты видны и слышны в эфире:
/// отдельная сцена-оверлей, browser_source с виджетом, звук в микшере OBS,
/// и оверлей поверх каждой пользовательской сцены.
pub(crate) async fn reconcile_donationalerts(st: &AppState) -> Result<Value> {
    let cfg = &st.cfg.donationalerts;
    let widget_url = load_secret("donationalerts_widget_url")?
        .ok_or_else(|| anyhow!("DonationAlerts Alerts Widget URL ещё не настроен"))?;

    let scene_list = st.obs.request("GetSceneList", json!({})).await?;
    let has_overlay_scene = scene_list
        .get("scenes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|s| s.get("sceneName").and_then(Value::as_str) == Some(&cfg.overlay_scene_name));
    if !has_overlay_scene {
        st.obs
            .request("CreateScene", json!({"sceneName": cfg.overlay_scene_name}))
            .await?;
    }

    let video = st
        .obs
        .request("GetVideoSettings", json!({}))
        .await
        .unwrap_or_else(|_| json!({"baseWidth": 1920, "baseHeight": 1080}));
    let width = video
        .get("baseWidth")
        .and_then(Value::as_i64)
        .unwrap_or(1920);
    let height = video
        .get("baseHeight")
        .and_then(Value::as_i64)
        .unwrap_or(1080);

    // reroute_audio обязателен: без него звук алерта идёт мимо микшера OBS,
    // и зрители его не услышат.
    let settings = json!({
        "url": widget_url,
        "width": width,
        "height": height,
        "reroute_audio": cfg.reroute_audio,
        "shutdown": false,
        "restart_when_active": false,
    });

    let inputs = st.obs.request("GetInputList", json!({})).await?;
    let existing = inputs
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("inputName").and_then(Value::as_str) == Some(&cfg.input_name))
        .cloned();

    // Шаг 1: источник существует и его настройки верны.
    let created = match existing {
        Some(inp) => {
            if inp.get("inputKind").and_then(Value::as_str) != Some("browser_source") {
                return Err(anyhow!(
                    "Имя {} уже занято источником другого типа. Переименуйте его в OBS или измените input_name в config/host.json.",
                    cfg.input_name
                ));
            }
            st.obs
                .request(
                    "SetInputSettings",
                    json!({"inputName": cfg.input_name, "inputSettings": settings, "overlay": true}),
                )
                .await?;
            false
        }
        None => {
            st.obs
                .request(
                    "CreateInput",
                    json!({
                        "sceneName": cfg.overlay_scene_name,
                        "inputName": cfg.input_name,
                        "inputKind": "browser_source",
                        "inputSettings": settings,
                        "sceneItemEnabled": true,
                    }),
                )
                .await?;
            true
        }
    };

    // Шаг 2: источник лежит именно в сцене-оверлее.
    //
    // Существование input проверяется глобально и ничего не говорит о том,
    // есть ли он в RSC_OVERLAYS. Если элемент удалили из сцены руками, прежний
    // код обновлял настройки и рапортовал об успехе, а оверлея в эфире не было.
    let (overlay_item_id, item_restored) =
        ensure_scene_item(&st.obs, &cfg.overlay_scene_name, &cfg.input_name).await?;
    st.obs
        .request(
            "SetSceneItemEnabled",
            json!({
                "sceneName": cfg.overlay_scene_name,
                "sceneItemId": overlay_item_id,
                "sceneItemEnabled": true,
            }),
        )
        .await
        .ok();

    // Шаг 3: звук.
    //
    // Снятие mute выполняем всегда, а не только при создании. Иначе достаточно
    // один раз заглушить источник — и reconcile будет успешно завершаться,
    // оставляя донаты беззвучными. Именно ради слышимости всё и затевалось.
    //
    // Громкость тоже восстанавливаем всегда: reconcile здесь обещает вернуть
    // DonationAlerts в рабочее эфирное состояние после ручной порчи настроек.
    st.obs.batch(vec![
        BatchItem::new(
            "SetInputMute",
            json!({"inputName": cfg.input_name, "inputMuted": false}),
        ),
        BatchItem::new(
            "SetInputVolume",
            json!({"inputName": cfg.input_name, "inputVolumeMul": db_to_mul(cfg.initial_volume_db)}),
        ),
        // Мониторинг берём из настройки, а не выключаем жёстко.
        //
        // Раньше здесь всегда стоял NONE, и поле `monitoring` в host.json
        // ничего не делало. Хуже того, выбор был неверным для тех, ради кого
        // программа написана: зрячий стример видит алерт на экране, а незрячий
        // не видит ничего, и звук — его единственный способ узнать о донате.
        BatchItem::new(
            "SetInputAudioMonitorType",
            json!({"inputName": cfg.input_name, "monitorType": monitor_type(&cfg.monitoring)}),
        ),
    ])
    .await
    .ok();

    // Шаг 4: оверлей поверх каждой пользовательской сцены.
    let mut added = 0;
    let mut present = 0;
    if cfg.enforce_overlays {
        let scene_list = st.obs.request("GetSceneList", json!({})).await?;
        let scenes: Vec<String> = scene_list
            .get("scenes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| {
                s.get("sceneName")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|s| s != &cfg.overlay_scene_name)
            .collect();

        for scene in scenes {
            let (id, was_created) =
                ensure_scene_item(&st.obs, &scene, &cfg.overlay_scene_name).await?;
            if was_created {
                added += 1;
            } else {
                present += 1;
            }
            raise_scene_item_to_top(&st.obs, &scene, id).await.ok();
        }
    }

    // Проверяем, а не верим на слово.
    //
    // Все команды выше отправлены, но их результат частично гасился через
    // .ok(), а внутри пакета у каждого запроса свой requestStatus. Без
    // перечитывания «ready: true» означало бы всего лишь «мы отправили
    // команды» — тот же ложный успех, от которого избавлялись в остальном.
    let verified = verify_donationalerts(st, cfg).await;

    Ok(json!({
        // ready только когда всё проверено и всё в порядке: непроверенное
        // за исправное здесь не выдаётся.
        "ready": verified.is_ready(),
        "problems": verified.problems,
        "overlay_scene": cfg.overlay_scene_name,
        "input_name": cfg.input_name,
        "widget_url": "configured",
        "reroute_audio": cfg.reroute_audio,
        "input_created": created,
        "overlay_item_restored": item_restored,
        "scenes_already_had_overlay": present,
        "scenes_added_overlay": added,
    }))
}

/// Перечитывает состояние OBS и говорит, что удалось установить.
///
/// Возвращает вердикты, а не строки. Прежде результат был списком текстов, и
/// вызывающий разбирал их подстрокой: сообщение «не удалось проверить сцену»
/// не содержало слова «нет оверлея», поэтому ошибка проверки превращалась в
/// «оверлей на месте». Теперь неудача проверки — отдельный исход.
pub(crate) struct DonationVerification {
    /// Не заглушен ли источник.
    pub audible: preflight::Verdict,
    /// Есть ли он в оверлейной сцене и виден ли там.
    pub in_overlay_scene: preflight::Verdict,
    /// Лежит ли оверлей верхним слоем во всех пользовательских сценах.
    pub on_top_everywhere: preflight::Verdict,
    /// Человеческие формулировки для панели и лога.
    pub problems: Vec<String>,
}

impl DonationVerification {
    /// Донаты дойдут до зрителей только если проверено всё и всё в порядке.
    pub fn is_ready(&self) -> bool {
        use preflight::Verdict::Ok;
        self.audible == Ok && self.in_overlay_scene == Ok && self.on_top_everywhere == Ok
    }
}

/// Ниже этой громкости считаем, что источник не звучит.
///
/// Ровно нулём не ограничиваемся: ползунок, уведённый в самый низ, даёт не
/// абсолютный ноль, а исчезающе малую величину, и сравнение с нулём объявило
/// бы такой источник звучащим.
const SILENT_VOLUME_MUL: f64 = 0.0001;

/// Дойдёт ли звук доната до зрителей.
///
/// Проверяем все четыре условия, а не одно. Прежде смотрели только на mute, и
/// `ready: true` мог означать что угодно: ползунок громкости в нуле, звук
/// страницы, идущий мимо микшера OBS, или мониторинг «только мне», при котором
/// алерт слышит владелец, а зрители — нет. Каждое из этих состояний ломает
/// донаты полностью и молча, а проверка их не замечала.
///
/// Именно ради этого проект перечитывает OBS вместо того, чтобы верить
/// отправленным командам: команды уходят пакетом, и любая из них может не
/// выполниться.
async fn audible_verdict(
    st: &AppState,
    cfg: &DonationAlertsConfig,
    problems: &mut Vec<String>,
) -> preflight::Verdict {
    use preflight::Verdict;
    let name = &cfg.input_name;
    let mut verdict = Verdict::Ok;

    // Заглушен ли источник.
    match st
        .obs
        .request("GetInputMute", json!({"inputName": name}))
        .await
    {
        Ok(v) => match v.get("inputMuted").and_then(Value::as_bool) {
            Some(true) => {
                problems.push(format!("{name} заглушен — донатов не будет слышно"));
                verdict = Verdict::Broken;
            }
            Some(false) => {}
            None => {
                problems.push(format!("OBS не сообщил состояние звука {name}"));
                verdict = verdict.min_known(Verdict::Unknown);
            }
        },
        Err(e) => {
            problems.push(format!("не удалось проверить звук {name}: {e}"));
            verdict = verdict.min_known(Verdict::Unknown);
        }
    }

    // Не уведён ли ползунок громкости в тишину. Незаглушенный источник с
    // нулевой громкостью выглядит исправным и молчит.
    match st
        .obs
        .request("GetInputVolume", json!({"inputName": name}))
        .await
    {
        Ok(v) => match v.get("inputVolumeMul").and_then(Value::as_f64) {
            Some(mul) if mul < SILENT_VOLUME_MUL => {
                problems.push(format!("громкость {name} выведена в ноль"));
                verdict = Verdict::Broken;
            }
            Some(_) => {}
            None => {
                problems.push(format!("OBS не сообщил громкость {name}"));
                verdict = verdict.min_known(Verdict::Unknown);
            }
        },
        Err(e) => {
            problems.push(format!("не удалось проверить громкость {name}: {e}"));
            verdict = verdict.min_known(Verdict::Unknown);
        }
    }

    // Уходит ли звук в эфир. При мониторинге «только мне» алерт слышит
    // владелец, а зрители не слышат ничего — и снаружи это неотличимо от
    // исправной работы.
    match st
        .obs
        .request("GetInputAudioMonitorType", json!({"inputName": name}))
        .await
    {
        Ok(v) => match v.get("monitorType").and_then(Value::as_str) {
            Some(t) if t.contains("MONITOR_ONLY") => {
                problems.push(format!(
                    "{name} выводится только владельцу: зрители донатов не услышат"
                ));
                verdict = Verdict::Broken;
            }
            Some(_) => {}
            None => {
                problems.push(format!("OBS не сообщил тип мониторинга {name}"));
                verdict = verdict.min_known(Verdict::Unknown);
            }
        },
        Err(e) => {
            problems.push(format!("не удалось проверить мониторинг {name}: {e}"));
            verdict = verdict.min_known(Verdict::Unknown);
        }
    }

    // Ведётся ли звук страницы в микшер OBS. Без этого браузер-источник играет
    // алерт мимо OBS, и в эфир не попадает ничего.
    match st
        .obs
        .request("GetInputSettings", json!({"inputName": name}))
        .await
    {
        Ok(v) => {
            let settings = v.get("inputSettings");
            // Отсутствие ключа означает значение по умолчанию, а по умолчанию
            // обс не ведёт звук страницы в микшер.
            let reroute = settings
                .and_then(|s| s.get("reroute_audio"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !reroute {
                problems.push(format!(
                    "у {name} выключено ведение звука в OBS: алерт прозвучит мимо эфира"
                ));
                verdict = Verdict::Broken;
            }
            let url_set = settings
                .and_then(|s| s.get("url"))
                .and_then(Value::as_str)
                .is_some_and(|u| u.starts_with("https://"));
            if !url_set {
                problems.push(format!("у {name} не задана ссылка виджета"));
                verdict = Verdict::Broken;
            }
        }
        Err(e) => {
            problems.push(format!("не удалось прочитать настройки {name}: {e}"));
            verdict = verdict.min_known(Verdict::Unknown);
        }
    }

    verdict
}

pub(crate) async fn verify_donationalerts(
    st: &AppState,
    cfg: &DonationAlertsConfig,
) -> DonationVerification {
    use preflight::Verdict;
    let mut problems = Vec::new();

    let audible = audible_verdict(st, cfg, &mut problems).await;

    let in_overlay_scene =
        match ensure_scene_item_present(&st.obs, &cfg.overlay_scene_name, &cfg.input_name).await {
            Ok(Some(id)) => {
                match st
                    .obs
                    .request(
                        "GetSceneItemEnabled",
                        json!({"sceneName": cfg.overlay_scene_name, "sceneItemId": id}),
                    )
                    .await
                {
                    Ok(v) => match v.get("sceneItemEnabled").and_then(Value::as_bool) {
                        Some(true) => Verdict::Ok,
                        Some(false) => {
                            problems.push(format!("{} скрыт в сцене оверлея", cfg.input_name));
                            Verdict::Broken
                        }
                        None => {
                            problems.push("OBS не сообщил, виден ли оверлей".into());
                            Verdict::Unknown
                        }
                    },
                    Err(e) => {
                        problems.push(format!("не удалось проверить видимость оверлея: {e}"));
                        Verdict::Unknown
                    }
                }
            }
            Ok(None) => {
                problems.push(format!(
                    "{} отсутствует в сцене {}",
                    cfg.input_name, cfg.overlay_scene_name
                ));
                Verdict::Broken
            }
            Err(e) => {
                problems.push(format!("не удалось проверить сцену оверлея: {e}"));
                Verdict::Unknown
            }
        };

    let on_top_everywhere = if cfg.enforce_overlays {
        verify_overlay_on_top(st, cfg, &mut problems).await
    } else {
        Verdict::Ok
    };

    DonationVerification {
        audible,
        in_overlay_scene,
        on_top_everywhere,
        problems,
    }
}

/// Проверяет, что оверлей лежит верхним слоем в каждой пользовательской сцене.
///
/// Любая неудача опроса — Unknown, а не молчаливый пропуск: раньше сбой
/// GetSceneList выключал всю эту проверку целиком, и панель об этом не знала.
async fn verify_overlay_on_top(
    st: &AppState,
    cfg: &DonationAlertsConfig,
    problems: &mut Vec<String>,
) -> preflight::Verdict {
    use preflight::Verdict;

    let list = match st.obs.request("GetSceneList", json!({})).await {
        Ok(list) => list,
        Err(e) => {
            problems.push(format!("не удалось получить список сцен: {e}"));
            return Verdict::Unknown;
        }
    };
    let scenes: Vec<String> = list
        .get("scenes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            s.get("sceneName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|s| s != &cfg.overlay_scene_name)
        .collect();

    let mut verdict = Verdict::Ok;
    for scene in scenes {
        let items = match st
            .obs
            .request("GetSceneItemList", json!({"sceneName": scene}))
            .await
        {
            Ok(items) => items,
            Err(e) => {
                problems.push(format!("не удалось прочитать сцену «{scene}»: {e}"));
                verdict = verdict.min_known(Verdict::Unknown);
                continue;
            }
        };
        let Some(list) = items.get("sceneItems").and_then(Value::as_array) else {
            problems.push(format!("OBS не вернул содержимое сцены «{scene}»"));
            verdict = verdict.min_known(Verdict::Unknown);
            continue;
        };
        let overlay = list
            .iter()
            .find(|i| i.get("sourceName").and_then(Value::as_str) == Some(&cfg.overlay_scene_name));
        match overlay {
            None => {
                problems.push(format!("в сцене «{scene}» нет оверлея донатов"));
                verdict = Verdict::Broken;
            }
            Some(item) => {
                let index = item.get("sceneItemIndex").and_then(Value::as_i64);
                if index != Some(top_scene_item_index(list.len())) {
                    problems.push(format!(
                        "в сцене «{scene}» оверлей не верхним слоем — его перекроют"
                    ));
                    verdict = Verdict::Broken;
                }
            }
        }
    }
    verdict
}

/// Ищет элемент сцены, ничего не создавая.
pub(crate) async fn ensure_scene_item_present(
    obs: &ObsHandle,
    scene: &str,
    source: &str,
) -> Result<Option<i64>> {
    let items = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    Ok(items
        .get("sceneItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("sourceName").and_then(Value::as_str) == Some(source))
        .and_then(|i| i.get("sceneItemId").and_then(Value::as_i64)))
}

/// Возвращает id элемента сцены, создавая его при отсутствии.
/// Второе значение — признак того, что элемент пришлось создать.
pub(crate) async fn ensure_scene_item(
    obs: &ObsHandle,
    scene: &str,
    source: &str,
) -> Result<(i64, bool)> {
    let items = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    let found = items
        .get("sceneItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| i.get("sourceName").and_then(Value::as_str) == Some(source))
        .and_then(|i| i.get("sceneItemId").and_then(Value::as_i64));

    if let Some(id) = found {
        return Ok((id, false));
    }
    let created = obs
        .request(
            "CreateSceneItem",
            json!({"sceneName": scene, "sourceName": source, "sceneItemEnabled": true}),
        )
        .await?;
    let id = created
        .get("sceneItemId")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("OBS не вернул id созданного элемента сцены"))?;
    Ok((id, true))
}

/// Индекс верхнего слоя для сцены с указанным числом элементов.
///
/// В obs-websocket индекс 0 — это НИЗ списка источников, а не верх. Прежний код
/// ставил оверлею индекс 0 с комментарием «верхний слой», из-за чего донаты
/// уезжали под захват игры. Ошибка не ловилась ни компилятором, ни CI.
pub(crate) fn top_scene_item_index(item_count: usize) -> i64 {
    item_count.saturating_sub(1) as i64
}

/// Поднимает элемент сцены на самый верх.
pub(crate) async fn raise_scene_item_to_top(
    obs: &ObsHandle,
    scene: &str,
    item_id: i64,
) -> Result<()> {
    let items = obs
        .request("GetSceneItemList", json!({"sceneName": scene}))
        .await?;
    let count = items
        .get("sceneItems")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    obs.request(
        "SetSceneItemIndex",
        json!({
            "sceneName": scene,
            "sceneItemId": item_id,
            "sceneItemIndex": top_scene_item_index(count),
        }),
    )
    .await?;
    Ok(())
}

pub(crate) fn split_scopes(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn scopes_from_body(body: &Value, field: &str, fallback: &[String]) -> Vec<String> {
    if let Some(raw) = body.get(field).and_then(Value::as_str) {
        let scopes = split_scopes(raw);
        if !scopes.is_empty() {
            return scopes;
        }
    }
    if let Some(values) = body.get(field).and_then(Value::as_array) {
        let scopes: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .flat_map(split_scopes)
            .collect();
        if !scopes.is_empty() {
            return scopes;
        }
    }
    fallback.to_vec()
}

pub(crate) fn secret_string(name: &str) -> Option<String> {
    load_secret(name)
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

pub(crate) fn secret_scopes(name: &str, fallback: &[String]) -> Vec<String> {
    secret_string(name)
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .filter(|scopes| !scopes.is_empty())
        .unwrap_or_else(|| fallback.to_vec())
}

pub(crate) fn effective_twitch_config(st: &AppState) -> TwitchConfig {
    let mut cfg = st.cfg.twitch.clone();
    if let Some(client_id) = secret_string("twitch_client_id") {
        cfg.client_id = client_id;
    }
    cfg.scopes = secret_scopes("twitch_scopes", &cfg.scopes);
    cfg
}

pub(crate) fn effective_donationalerts_config(st: &AppState) -> DonationAlertsConfig {
    resolve_donationalerts_config(&st.cfg.donationalerts)
}

/// Накладывает сохранённые в хранилище секретов значения поверх host.json.
/// Вынесено из [`effective_donationalerts_config`], чтобы этим же путём мог
/// пользоваться фоновый воркер, у которого нет доступа к AppState.
pub(crate) fn resolve_donationalerts_config(base: &DonationAlertsConfig) -> DonationAlertsConfig {
    let mut cfg = base.clone();
    if let Some(client_id) = secret_string("donationalerts_client_id") {
        cfg.client_id = client_id;
    }
    if let Some(secret) = secret_string("donationalerts_client_secret") {
        cfg.client_secret = secret;
    }
    if let Some(redirect_uri) = secret_string("donationalerts_redirect_uri") {
        cfg.redirect_uri = redirect_uri;
    }
    cfg.oauth_scopes = secret_scopes("donationalerts_oauth_scopes", &cfg.oauth_scopes);
    cfg
}

pub(crate) fn save_host_config_update(update: impl FnOnce(&mut HostConfig)) -> Result<()> {
    let mut cfg = load_host_config()?;
    update(&mut cfg);
    save_json(&config_dir().join("host.json"), &cfg)
}

pub(crate) async fn da_oauth_start(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    require_auth(&st, &headers).await?;
    let c = effective_donationalerts_config(&st);
    if c.client_id.is_empty() || c.client_secret.is_empty() || c.redirect_uri.is_empty() {
        return Err(bad(
            "Сначала сохраните DonationAlerts client_id, client_secret и redirect URI в веб-панели.",
        ));
    }
    let state_token = random_secret_b64(16);
    *st.oauth_state.write().await = Some(state_token.clone());
    let url = format!(
        "https://www.donationalerts.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        urlencoding::encode(&c.client_id),
        urlencoding::encode(&c.redirect_uri),
        urlencoding::encode(&c.oauth_scopes.join(" ")),
        urlencoding::encode(&state_token),
    );
    Ok(Json(json!({"authorize_url": url})))
}

/// Запоминает исход подключения и объявляет его панели.
///
/// Прежде любая неудача возвращалась строчкой в постороннюю вкладку браузера и
/// нигде больше не появлялась: ни в логе, ни в панели. Для незрячего владельца
/// это означало «нажал подключить — и ничего», причём без единой зацепки, что
/// пошло не так. Теперь исход виден в трёх местах: в логе, в состоянии
/// DonationAlerts и вслух через поток событий.
async fn finish_oauth(st: &AppState, ok: bool, note: String) -> Response {
    if ok {
        info!("DonationAlerts OAuth: {note}");
    } else {
        warn!("DonationAlerts OAuth: {note}");
    }
    *st.donationalerts_oauth_note.write().await = Some(note.clone());
    let _ = st.events.send(json!({
        "type": "donationalerts_status",
        "oauth_ok": ok,
        "message": note.clone(),
    }));
    let title = if ok {
        "DonationAlerts подключён"
    } else {
        "DonationAlerts: подключиться не удалось"
    };
    Html(format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <title>{title}</title></head><body><h1>{title}</h1>\
         <p role=\"status\">{}</p>\
         <p>Эту вкладку можно закрыть: то же сообщение показано в панели.</p>\
         </body></html>",
        html_escape_text(&note)
    ))
    .into_response()
}

fn html_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Короткая выжимка из ответа token endpoint для человека.
///
/// Тело берём целиком только при неудаче: при успехе там лежит access_token,
/// которому не место ни в логе, ни на странице.
fn token_error_detail(body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    for key in ["message", "error_description", "error"] {
        if let Some(text) = parsed.get(key).and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return text.trim().to_string();
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "пустой ответ".to_string()
    } else {
        trimmed.chars().take(300).collect()
    }
}

pub(crate) async fn da_oauth_callback(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // DonationAlerts сообщает об отказе не кодом, а параметрами error*.
    // Прежде мы этого не замечали и жаловались на отсутствие code, пряча
    // настоящую причину — например, неподходящий redirect_uri.
    if let Some(error) = q.get("error") {
        let detail = q
            .get("error_description")
            .map(String::as_str)
            .unwrap_or(error.as_str());
        return finish_oauth(
            &st,
            false,
            format!("DonationAlerts отказал: {detail}. Проверьте, что Redirect URI в настройках приложения DonationAlerts совпадает с указанным в панели буква в букву."),
        )
        .await;
    }

    let Some(code) = q.get("code") else {
        return finish_oauth(
            &st,
            false,
            "В ответе нет authorization code. Начните подключение заново из панели.".to_string(),
        )
        .await;
    };

    // Проверка state: без неё кто угодно в tailnet мог бы подсунуть чужой код.
    let expected = st.oauth_state.write().await.take();
    match (expected, q.get("state")) {
        (Some(expected), Some(got)) if secret_eq(&expected, got) => {}
        (None, _) => {
            return finish_oauth(
                &st,
                false,
                "Ответ пришёл, но подключение не начиналось или уже было завершено. \
                 Нажмите «Подключить DonationAlerts» и пройдите вход заново."
                    .to_string(),
            )
            .await;
        }
        _ => {
            return finish_oauth(
                &st,
                false,
                "Проверка state не пройдена: ответ не относится к начатому подключению. \
                 Начните заново из панели."
                    .to_string(),
            )
            .await;
        }
    }

    let c = effective_donationalerts_config(&st);
    let form = [
        ("grant_type", "authorization_code"),
        ("client_id", c.client_id.as_str()),
        ("client_secret", c.client_secret.as_str()),
        ("redirect_uri", c.redirect_uri.as_str()),
        ("code", code.as_str()),
    ];
    let response = match st
        .http
        .post("https://www.donationalerts.com/oauth/token")
        .form(&form)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return finish_oauth(
                &st,
                false,
                format!("Сервер токенов DonationAlerts недоступен: {e}"),
            )
            .await;
        }
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);

    if parsed.get("access_token").is_none() {
        // Именно здесь пряталась причина: DonationAlerts объясняет отказ в теле
        // ответа, а прежний код это тело выбрасывал.
        return finish_oauth(
            &st,
            false,
            format!(
                "DonationAlerts вернул {} и не выдал токен: {}",
                status.as_u16(),
                token_error_detail(&body)
            ),
        )
        .await;
    }

    match save_secret("donationalerts_tokens", &parsed.to_string()) {
        Ok(()) => {
            finish_oauth(
                &st,
                true,
                "Подключение выполнено, токен сохранён.".to_string(),
            )
            .await
        }
        Err(e) => {
            finish_oauth(
                &st,
                false,
                format!("Токен получен, но сохранить не удалось: {e}"),
            )
            .await
        }
    }
}

#[cfg(test)]
mod oauth_tests {
    use super::*;

    #[test]
    fn error_message_is_taken_from_the_response() {
        // Ровно то, что прячет обычный отказ DonationAlerts.
        assert_eq!(
            token_error_detail(
                r#"{"error":"invalid_client","message":"Client authentication failed"}"#
            ),
            "Client authentication failed"
        );
        assert_eq!(
            token_error_detail(r#"{"error_description":"Redirect URI mismatch"}"#),
            "Redirect URI mismatch"
        );
        // Когда описания нет, годится и краткий код ошибки.
        assert_eq!(
            token_error_detail(r#"{"error":"invalid_grant"}"#),
            "invalid_grant"
        );
    }

    #[test]
    fn non_json_and_empty_bodies_still_say_something() {
        // Прокси или страница-заглушка вместо JSON не должны оставлять
        // владельца с пустым сообщением.
        assert_eq!(token_error_detail("  "), "пустой ответ");
        assert_eq!(
            token_error_detail("<html>502 Bad Gateway</html>"),
            "<html>502 Bad Gateway</html>"
        );
    }

    #[test]
    fn long_bodies_are_cut_to_stay_readable() {
        let detail = token_error_detail(&"x".repeat(5000));
        assert_eq!(detail.chars().count(), 300);
    }

    #[test]
    fn markup_in_the_message_cannot_reach_the_page_as_markup() {
        // Текст ответа приходит из сети и попадает на страницу обратного вызова.
        assert_eq!(
            html_escape_text("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }
}

#[cfg(test)]
mod monitoring_tests {
    use super::*;

    #[test]
    fn owner_hears_donations_by_default() {
        // Пустая или незнакомая настройка не должна оставлять незрячего
        // владельца без звука: это худший исход опечатки.
        for value in ["", "both", "on", "yes", "чепуха", "MONITOR_AND_OUTPUT"] {
            assert_eq!(
                monitor_type(value),
                "OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT",
                "значение: {value:?}"
            );
        }
    }

    #[test]
    fn silence_for_the_owner_must_be_asked_for_explicitly() {
        for value in ["off", "none", "OFF", " None ", "output"] {
            assert_eq!(
                monitor_type(value),
                "OBS_MONITORING_TYPE_NONE",
                "значение: {value:?}"
            );
        }
    }

    #[test]
    fn monitor_only_keeps_alerts_out_of_the_stream() {
        for value in ["monitor", "monitor_only", "Monitor_Only"] {
            assert_eq!(
                monitor_type(value),
                "OBS_MONITORING_TYPE_MONITOR_ONLY",
                "значение: {value:?}"
            );
        }
    }
}
