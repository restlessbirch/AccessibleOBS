//! Чтение чата Twitch.
//!
//! Читаем анонимно: Twitch пускает в чат гостя без токена, если представиться
//! ником вида `justinfan<число>`. Поэтому чат работает даже когда OAuth ещё
//! не пройден, и никаких прав на аккаунт для него не нужно.
//!
//! Зачем свой клиент вместо официального встраиваемого виджета. Виджет — чужой
//! iframe: экранный диктор читает его со скрипом, а новые сообщения не
//! объявляются вовсе, и незрячему приходится самому лазить туда курсором.
//! Свой клиент превращает сообщения в обычные события, которые панель и экран
//! актёра могут зачитывать вслух сразу.
//!
//! Разбор строк IRC вынесен в чистую функцию и покрыт тестами: протокол
//! текстовый, и ошибиться в нём легко.

use serde::Serialize;

/// Одно сообщение чата.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatMessage {
    /// Отображаемое имя автора.
    pub author: String,
    pub text: String,
}

impl ChatMessage {
    /// Строка для зачитывания вслух.
    pub fn spoken(&self) -> String {
        format!("{}: {}", self.author, self.text)
    }
}

/// Что означает пришедшая строка протокола.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Сообщение в чате.
    Message(ChatMessage),
    /// Сервер проверяет, живы ли мы. Ответить обязаны, иначе отключит.
    Ping(String),
    /// Всё остальное: приветствия, списки участников, служебное.
    Other,
}

/// Разбирает одну строку протокола IRC.
///
/// Формат сообщения:
/// `@tags :nick!nick@nick.tmi.twitch.tv PRIVMSG #канал :текст`
///
/// Теги необязательны и присылаются только если их запросить; из них берём
/// `display-name`, потому что в нике теряется регистр и не-латиница.
pub fn parse_line(line: &str) -> Incoming {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return Incoming::Other;
    }

    // PING приходит без префикса и требует немедленного PONG.
    if let Some(token) = line.strip_prefix("PING ") {
        return Incoming::Ping(token.to_string());
    }

    let (tags, rest) = match line.strip_prefix('@') {
        Some(with_tags) => match with_tags.split_once(' ') {
            Some((tags, rest)) => (Some(tags), rest),
            None => return Incoming::Other,
        },
        None => (None, line),
    };

    let Some(rest) = rest.strip_prefix(':') else {
        return Incoming::Other;
    };
    let Some((prefix, command)) = rest.split_once(' ') else {
        return Incoming::Other;
    };
    // Нас интересуют только сообщения; JOIN, PART и прочее пропускаем.
    let Some(after) = command.strip_prefix("PRIVMSG ") else {
        return Incoming::Other;
    };
    // `#канал :текст` — текст всегда после первого двоеточия и может
    // содержать любые символы, включая пробелы и ещё одно двоеточие.
    let Some((_channel, text)) = after.split_once(" :") else {
        return Incoming::Other;
    };

    let author = tags
        .and_then(display_name_from_tags)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| nick_from_prefix(prefix).to_string());

    Incoming::Message(ChatMessage {
        author,
        text: text.to_string(),
    })
}

fn display_name_from_tags(tags: &str) -> Option<String> {
    tags.split(';')
        .find_map(|pair| pair.strip_prefix("display-name="))
        .map(unescape_tag)
}

/// В тегах IRCv3 пробелы и точки с запятой экранируются.
fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

fn nick_from_prefix(prefix: &str) -> &str {
    prefix.split('!').next().unwrap_or(prefix)
}

/// Гостевой ник для анонимного входа.
pub fn anonymous_nick(seed: u32) -> String {
    // Twitch требует именно префикс justinfan; число произвольное.
    format!("justinfan{}", 10_000 + seed % 80_000)
}

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

const CHAT_URL: &str = "wss://irc-ws.chat.twitch.tv:443";
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Пока канал неизвестен, ждать долго: это не ошибка, а просто
/// неподключённый Twitch.
const IDLE_POLL: Duration = Duration::from_secs(30);

/// Способ узнать текущий канал. Функция, а не строка: владелец может
/// подключить Twitch уже после запуска агента.
pub type ChannelSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Держит подписку на чат живой и раздаёт сообщения в общий поток событий.
pub fn spawn(channel: ChannelSource, events: broadcast::Sender<Value>) {
    tokio::spawn(async move {
        let mut backoff = RETRY_MIN;
        loop {
            let Some(login) = channel() else {
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            };
            match session(&login, &events).await {
                Ok(()) => {
                    info!("Twitch-чат: соединение закрыто, переподключаюсь");
                    backoff = RETRY_MIN;
                }
                Err(e) => warn!("Twitch-чат: {e:#}"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RETRY_MAX);
        }
    });
}

async fn session(login: &str, events: &broadcast::Sender<Value>) -> anyhow::Result<()> {
    let (ws, _) = connect_async(CHAT_URL).await?;
    let (mut sink, mut stream) = ws.split();

    // Анонимный вход: пароль формальный, ник обязан начинаться с justinfan.
    let nick = anonymous_nick(rand::random::<u32>());
    sink.send(Message::Text("PASS SCHMOOPIIE".into())).await?;
    sink.send(Message::Text(format!("NICK {nick}").into()))
        .await?;
    // Просим теги: без них вместо отображаемого имени придёт голый ник.
    sink.send(Message::Text(
        "CAP REQ :twitch.tv/tags twitch.tv/commands".into(),
    ))
    .await?;
    sink.send(Message::Text(
        format!("JOIN #{}", login.to_lowercase()).into(),
    ))
    .await?;
    info!("Twitch-чат: подключён к каналу {login}");

    while let Some(msg) = stream.next().await {
        let Message::Text(text) = msg? else { continue };
        // В одном кадре может прийти несколько строк.
        for line in text.lines() {
            match parse_line(line) {
                Incoming::Message(message) => {
                    let _ = events.send(json!({
                        "type": "chat",
                        "author": message.author,
                        "text": message.text,
                        "spoken": message.spoken(),
                    }));
                }
                // Не ответить — значит быть отключённым через пару минут.
                Incoming::Ping(token) => {
                    sink.send(Message::Text(format!("PONG {token}").into()))
                        .await?;
                }
                Incoming::Other => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(line: &str) -> ChatMessage {
        match parse_line(line) {
            Incoming::Message(m) => m,
            other => panic!("ожидалось сообщение, получено {other:?}"),
        }
    }

    #[test]
    fn plain_message_is_parsed() {
        let m = message(":vasya!vasya@vasya.tmi.twitch.tv PRIVMSG #channel :привет всем");
        assert_eq!(m.author, "vasya");
        assert_eq!(m.text, "привет всем");
    }

    #[test]
    fn display_name_wins_over_nick() {
        // В нике теряются регистр и кириллица, а зачитывать надо то, что
        // зрители видят у себя.
        let m = message(
            "@display-name=Вася;mod=0 :vasya!vasya@vasya.tmi.twitch.tv PRIVMSG #channel :текст",
        );
        assert_eq!(m.author, "Вася");
    }

    #[test]
    fn empty_display_name_falls_back_to_nick() {
        let m = message("@display-name= :vasya!vasya@vasya.tmi.twitch.tv PRIVMSG #ch :текст");
        assert_eq!(m.author, "vasya");
    }

    #[test]
    fn escaped_characters_in_display_name_are_restored() {
        let m = message("@display-name=Иван\\sПетров :i!i@i.tmi.twitch.tv PRIVMSG #ch :ага");
        assert_eq!(m.author, "Иван Петров");
    }

    #[test]
    fn colons_inside_text_survive() {
        // Ссылки и время в сообщении не должны обрезаться по первому двоеточию.
        let m = message(":a!a@a.tmi.twitch.tv PRIVMSG #ch :смотри: https://example.com в 12:30");
        assert_eq!(m.text, "смотри: https://example.com в 12:30");
    }

    #[test]
    fn empty_text_is_still_a_message() {
        let m = message(":a!a@a.tmi.twitch.tv PRIVMSG #ch :");
        assert_eq!(m.text, "");
    }

    #[test]
    fn ping_is_recognised_and_carries_its_token() {
        // Не ответить на PING — значит быть отключённым через пару минут.
        assert_eq!(
            parse_line("PING :tmi.twitch.tv"),
            Incoming::Ping(":tmi.twitch.tv".into())
        );
    }

    #[test]
    fn service_lines_are_ignored() {
        for line in [
            ":tmi.twitch.tv 001 justinfan1 :Welcome, GLHF!",
            ":justinfan1!justinfan1@justinfan1.tmi.twitch.tv JOIN #channel",
            ":tmi.twitch.tv 353 justinfan1 = #channel :justinfan1",
            ":a!a@a.tmi.twitch.tv PART #channel",
            "",
            "мусор",
        ] {
            assert_eq!(parse_line(line), Incoming::Other, "строка: {line}");
        }
    }

    #[test]
    fn malformed_lines_do_not_panic() {
        for line in ["@", "@tags", ":", ":only-prefix", "@t :p PRIVMSG", "PING"] {
            let _ = parse_line(line);
        }
    }

    #[test]
    fn trailing_newlines_are_trimmed() {
        let m = message(":a!a@a.tmi.twitch.tv PRIVMSG #ch :текст\r\n");
        assert_eq!(m.text, "текст");
    }

    #[test]
    fn spoken_form_names_the_author_first() {
        let m = ChatMessage {
            author: "Вася".into(),
            text: "привет".into(),
        };
        assert_eq!(m.spoken(), "Вася: привет");
    }

    #[test]
    fn anonymous_nick_has_the_required_prefix() {
        for seed in [0, 1, 999_999] {
            let nick = anonymous_nick(seed);
            assert!(nick.starts_with("justinfan"), "{nick}");
            assert!(nick.len() > "justinfan".len());
        }
    }
}
