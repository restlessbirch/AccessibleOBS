//! Интеграционные тесты клиента OBS против поддельного сервера.
//!
//! Юнит-тесты проверяют разбор отдельных сообщений, но самые дорогие ошибки
//! этого проекта жили не внутри функций, а на стыках: рукопожатие, отказ по
//! паролю, переподключение, соответствие ответа запросу. Здесь поднимается
//! настоящий WebSocket-сервер, говорящий на протоколе obs-websocket 5.x,
//! и клиент работает с ним как с OBS.

use futures_util::{SinkExt, StreamExt};
use remote_stream_control::obs::{BatchItem, ObsHandle, response_data};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const PASSWORD: &str = "тестовый-пароль";

/// Как поддельный OBS ведёт себя при подключении.
#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    /// Обычный OBS: принимает пароль и отвечает на запросы.
    Normal,
    /// Отклоняет авторизацию кодом 4009, как настоящий obs-websocket.
    RejectPassword,
    /// Обрывает соединение после первого запроса.
    DropAfterFirstRequest,
    /// Принимает запросы и молчит: сокет жив, ответов нет.
    NeverAnswer,
}

struct FakeObs {
    port: u16,
    /// Сколько раз к серверу подключались. Нужно, чтобы отличить
    /// переподключение от «клиент так и не вернулся».
    connections: Arc<AtomicU32>,
}

/// Поднимает поддельный OBS на свободном порту.
async fn start_fake_obs(behaviour: Behaviour) -> FakeObs {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("порт занят");
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicU32::new(0));
    let counter = connections.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve_one(stream, behaviour));
        }
    });

    FakeObs { port, connections }
}

async fn serve_one(stream: tokio::net::TcpStream, behaviour: Behaviour) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();

    // Hello: обязательно с challenge и salt, иначе клиент не станет считать
    // аутентификацию и мы не проверим главную часть рукопожатия.
    let hello = json!({
        "op": 0,
        "d": {
            "obsWebSocketVersion": "5.7.4",
            "rpcVersion": 1,
            "authentication": {"challenge": "вызов", "salt": "соль"},
        }
    });
    if sink
        .send(Message::Text(hello.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Identify
    let Some(Ok(Message::Text(identify))) = source.next().await else {
        return;
    };
    let identify: Value = serde_json::from_str(&identify).unwrap_or(Value::Null);
    let sent_auth = identify
        .pointer("/d/authentication")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if behaviour == Behaviour::RejectPassword || sent_auth.is_empty() {
        let frame = tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: 4009u16.into(),
            reason: "Authentication failed".into(),
        };
        let _ = sink.send(Message::Close(Some(frame))).await;
        return;
    }

    let _ = sink
        .send(Message::Text(
            json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})
                .to_string()
                .into(),
        ))
        .await;

    let mut answered = 0u32;
    while let Some(Ok(msg)) = source.next().await {
        let Message::Text(text) = msg else { continue };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let op = v.get("op").and_then(Value::as_i64);

        if behaviour == Behaviour::NeverAnswer {
            continue;
        }
        if behaviour == Behaviour::DropAfterFirstRequest && answered >= 1 {
            return;
        }

        let reply = match op {
            Some(6) => {
                let id = v
                    .pointer("/d/requestId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let request_type = v
                    .pointer("/d/requestType")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(single_response(id, request_type, &v))
            }
            Some(8) => {
                let id = v
                    .pointer("/d/requestId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let requests = v
                    .pointer("/d/requests")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let results: Vec<Value> = requests
                    .iter()
                    .map(|r| {
                        let rt = r.get("requestType").and_then(Value::as_str).unwrap_or("");
                        single_response("", rt, r)
                            .pointer("/d")
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect();
                Some(json!({"op": 9, "d": {"requestId": id, "results": results}}))
            }
            _ => None,
        };

        if let Some(reply) = reply {
            answered += 1;
            if sink
                .send(Message::Text(reply.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// Ответ на один запрос. Форма совпадает с настоящим obs-websocket.
fn single_response(id: &str, request_type: &str, request: &Value) -> Value {
    let data = match request_type {
        "GetVersion" => json!({"obsVersion": "32.2.1", "obsWebSocketVersion": "5.7.4"}),
        "GetCurrentProgramScene" => json!({"currentProgramSceneName": "Игра"}),
        // Эхо входных данных: так тест видит, что до сервера дошёл
        // именно тот запрос, который отправляли.
        "Echo" => request
            .pointer("/d/requestData")
            .or_else(|| request.get("requestData"))
            .cloned()
            .unwrap_or(json!({})),
        "Fail" => {
            return json!({"op": 7, "d": {
                "requestId": id,
                "requestStatus": {"result": false, "code": 604, "comment": "Сцена не найдена"},
            }});
        }
        _ => json!({}),
    };
    json!({"op": 7, "d": {
        "requestId": id,
        "requestStatus": {"result": true, "code": 100},
        "responseData": data,
    }})
}

fn connect(fake: &FakeObs) -> ObsHandle {
    ObsHandle::spawn("127.0.0.1", fake.port, PASSWORD)
}

/// Ждёт подключения клиента к OBS, чтобы тесты не зависели от скорости машины.
async fn wait_connected(obs: &ObsHandle, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if obs.is_connected().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::test]
async fn handshake_completes_and_reports_version() {
    let fake = start_fake_obs(Behaviour::Normal).await;
    let obs = connect(&fake);

    assert!(
        wait_connected(&obs, Duration::from_secs(5)).await,
        "не подключился"
    );

    // Версия приходит отдельным запросом уже после рукопожатия.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if obs.status().await.obs_version.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "версия так и не пришла"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let status = obs.status().await;
    assert_eq!(status.obs_version.as_deref(), Some("32.2.1"));
    assert_eq!(status.websocket_version.as_deref(), Some("5.7.4"));
}

#[tokio::test]
async fn request_and_response_are_matched_by_id() {
    // Ответы приходят вперемешку, и клиент обязан разложить их по
    // запросам. Ошибка здесь означала бы, что панель показывает
    // громкость одного источника под именем другого.
    let fake = start_fake_obs(Behaviour::Normal).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    let first = obs.request("Echo", json!({"метка": 1}));
    let second = obs.request("Echo", json!({"метка": 2}));
    let third = obs.request("Echo", json!({"метка": 3}));
    let (a, b, c) = tokio::join!(first, second, third);

    assert_eq!(a.unwrap()["метка"], 1);
    assert_eq!(b.unwrap()["метка"], 2);
    assert_eq!(c.unwrap()["метка"], 3);
}

#[tokio::test]
async fn obs_refusal_reaches_the_caller() {
    let fake = start_fake_obs(Behaviour::Normal).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    let err = obs
        .request("Fail", json!({}))
        .await
        .expect_err("OBS отказал, значит и вызывающий должен получить ошибку");
    assert!(err.to_string().contains("Сцена не найдена"), "{err}");
}

#[tokio::test]
async fn batch_preserves_order_of_results() {
    // Список аудио опирается на то, что i-й результат отвечает i-му запросу.
    // Если порядок съедет, громкость окажется приписана чужому источнику.
    let fake = start_fake_obs(Behaviour::Normal).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    let results = obs
        .batch(vec![
            BatchItem::new("Echo", json!({"n": "первый"})),
            BatchItem::new("Echo", json!({"n": "второй"})),
            BatchItem::new("Echo", json!({"n": "третий"})),
        ])
        .await
        .expect("пакет выполнен");

    assert_eq!(results.len(), 3);
    assert_eq!(response_data(&results[0]).unwrap()["n"], "первый");
    assert_eq!(response_data(&results[1]).unwrap()["n"], "второй");
    assert_eq!(response_data(&results[2]).unwrap()["n"], "третий");
}

#[tokio::test]
async fn empty_batch_does_not_reach_obs() {
    // Свежая установка OBS без источников — обычное дело на первом запуске.
    let fake = start_fake_obs(Behaviour::Normal).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    assert!(
        obs.batch(vec![])
            .await
            .expect("пустой пакет допустим")
            .is_empty()
    );
}

#[tokio::test]
async fn wrong_password_is_explained_not_just_logged() {
    // 4009 — единственный отказ, который пользователь может исправить сам,
    // поэтому текст обязан называть и причину, и что запустить.
    let fake = start_fake_obs(Behaviour::RejectPassword).await;
    let obs = connect(&fake);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut message = None;
    while tokio::time::Instant::now() < deadline {
        if let Some(err) = obs.status().await.error {
            message = Some(err);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let message = message.expect("ошибка авторизации должна попасть в статус");
    assert!(message.contains("пароль"), "{message}");
    assert!(message.contains("START_FRIEND.bat"), "{message}");
    assert!(!obs.is_connected().await);
}

#[tokio::test]
async fn client_reconnects_after_a_dropped_connection() {
    // Актёр закрыл и снова открыл OBS — панель обязана ожить сама.
    let fake = start_fake_obs(Behaviour::DropAfterFirstRequest).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    // Первый запрос сервер обслужит, после чего оборвёт соединение.
    let _ = obs.request("Echo", json!({"n": 1})).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fake.connections.load(Ordering::SeqCst) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "клиент не переподключился"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn commands_fail_fast_while_obs_is_down() {
    // Пока связи нет, кнопки панели должны отвечать сразу, а не висеть
    // до таймаута: оператор иначе решит, что панель зависла.
    let obs = ObsHandle::spawn("127.0.0.1", 1, PASSWORD); // порт, где никого нет
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = std::time::Instant::now();
    let err = obs
        .request("Echo", json!({}))
        .await
        .expect_err("без связи запрос обязан провалиться");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "ждали {:?}, а должны были отказать сразу",
        started.elapsed()
    );
    assert!(err.to_string().contains("OBS"), "{err}");
}

#[tokio::test]
async fn hung_obs_does_not_hang_the_caller_forever() {
    // Сокет жив, ответов нет. Вызывающий обязан уйти по таймауту.
    let fake = start_fake_obs(Behaviour::NeverAnswer).await;
    let obs = connect(&fake);
    assert!(wait_connected(&obs, Duration::from_secs(5)).await);

    let started = std::time::Instant::now();
    let err = obs
        .request("Echo", json!({}))
        .await
        .expect_err("молчащий OBS обязан привести к ошибке");
    assert!(err.to_string().contains("не ответил"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "таймаут не сработал: {:?}",
        started.elapsed()
    );
}
