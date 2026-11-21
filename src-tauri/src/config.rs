use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{fs, io::Write, path::Path};

use crate::claude::{safe_args, LEGACY_ARGS};

const PROMPT: &str = "Ты суфлёр в живом разговоре. Тебе присылают расшифровку речи собеседника, в ней бывают ошибки распознавания, додумывай по смыслу. Если там вопрос или просьба, сразу дай готовый ответ, который можно произнести вслух: 1-3 коротких предложения, без вступлений и без пересказа вопроса. Если просят код или решить задачу, дай короткое рабочее решение и одну строку пояснения. Если вопроса нет, ответь одной точкой. Отвечай на языке собеседника. Не используй инструменты, не читай и не меняй файлы.";
const WITHOUT_WEB: &str = "Не используй инструменты, не читай и не меняй файлы.";
const WITH_WEB: &str = "Ищи в сети только свежие факты.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    pub whisper_model: String,
    pub whisper_threads: u32,
    pub whisper_url: String,
    pub language: String,
    pub source: String,
    pub auto_send: bool,
    pub only_questions: bool,
    pub silence_ms: u32,
    pub sensitivity: f32,
    pub opacity: f64,
    pub font_size: u32,
    pub claude_command: String,
    pub claude_args: String,
    pub system_prompt: String,
    pub message_template: String,
    pub bounds: Option<Bounds>,
    pub web_search: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            whisper_model: "small-q5_1".into(),
            whisper_threads: 0,
            whisper_url: String::new(),
            language: "ru".into(),
            source: "system".into(),
            auto_send: true,
            only_questions: true,
            silence_ms: 700,
            sensitivity: 1.0,
            opacity: 0.97,
            font_size: 14,
            claude_command: "claude".into(),
            claude_args: "--model haiku".into(),
            system_prompt: PROMPT.into(),
            message_template: "Собеседник: «{text}»".into(),
            bounds: None,
            web_search: false,
        }
    }
}

impl Config {
    pub(crate) fn apply(&self, input: &Map<String, Value>) -> Result<(Self, Vec<String>)> {
        let defaults = serde_json::to_value(Self::default())?;
        let mut value = serde_json::to_value(self)?;
        let mut warnings = Vec::new();
        for (key, raw) in input {
            let Some(default) = defaults.get(key) else {
                continue;
            };
            let v = match (key.as_str(), raw.as_f64()) {
                ("whisperThreads", Some(n)) => Value::from(n.clamp(0., 32.) as u32),
                ("silenceMs", Some(n)) => Value::from(n.clamp(200., 4000.) as u32),
                ("fontSize", Some(n)) => Value::from(n.clamp(9., 30.) as u32),
                ("opacity", Some(n)) => Value::from(n.clamp(0.3, 1.)),
                ("sensitivity", Some(n)) => Value::from(n.clamp(0.2, 6.)),
                _ => raw.clone(),
            };
            value[key] = v;
            if serde_json::from_value::<Self>(value.clone()).is_err() {
                warnings.push(format!(
                    "config.json: неверный тип {key}, взято значение по умолчанию"
                ));
                value[key] = default.clone();
            }
        }
        let mut cfg: Self = serde_json::from_value(value)?;
        cfg.whisper_model = cfg
            .whisper_model
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
            .collect();
        if cfg.whisper_model.is_empty() {
            cfg.whisper_model = Self::default().whisper_model;
        }
        if cfg.claude_args.trim() == LEGACY_ARGS {
            cfg.claude_args = "--model haiku".into();
        }
        match safe_args(&cfg.claude_args) {
            Ok((args, w)) if !w.is_empty() => {
                cfg.claude_args = args
                    .iter()
                    .map(|v| {
                        if v.is_empty()
                            || v.contains(char::is_whitespace)
                            || v.contains(['\'', '"'])
                        {
                            if !v.contains('\'') {
                                format!("'{v}'")
                            } else {
                                format!("\"{}\"", v.replace('"', "\\\""))
                            }
                        } else {
                            v.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                warnings.extend(w);
            }
            Ok(_) => {}
            Err(err) => {
                warnings.push(format!("{err}, взято --model haiku"));
                cfg.claude_args = Self::default().claude_args;
            }
        }
        let web_prompt = PROMPT.replace(WITHOUT_WEB, WITH_WEB);
        if cfg.web_search && cfg.system_prompt == PROMPT {
            cfg.system_prompt = web_prompt;
        } else if !cfg.web_search && cfg.system_prompt == web_prompt {
            cfg.system_prompt = PROMPT.into();
        }
        Ok((cfg, warnings))
    }

    pub(crate) fn load(dir: &Path) -> Result<(Self, Vec<String>)> {
        let path = dir.join("config.json");
        let text = match fs::read(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("не прочитал {}", path.display())),
        };
        let mut warnings = Vec::new();
        let mut cfg = Self::default();
        if let Some(bytes) = text {
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
            match serde_json::from_slice::<Map<String, Value>>(bytes) {
                Ok(map) => {
                    (cfg, warnings) = cfg.apply(&map)?;
                }
                Err(_) => {
                    fs::rename(&path, dir.join("config.bad.json"))
                        .context("не сохранил исходный config.json в config.bad.json")?;
                    warnings.push("config.json битый, копия в config.bad.json".into());
                }
            }
        }
        if let Err(e) = cfg.save(dir) {
            warnings.push(format!("{e:#}"));
        }
        Ok((cfg, warnings))
    }

    pub(crate) fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("config.json.tmp");
        let mut f = fs::File::create(&path)
            .with_context(|| format!("не открыл {} для записи", path.display()))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        f.write_all(&bytes)
            .and_then(|()| f.sync_all())
            .with_context(|| format!("не записал {}", path.display()))?;
        drop(f);
        fs::rename(&path, dir.join("config.json")).context("не заменил config.json")?;
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn patched(&self, mut patch: Map<String, Value>) -> Result<(Self, Vec<String>)> {
        patch.remove("bounds");
        let (cfg, w) = self.apply(&patch)?;
        if !cfg.whisper_url.trim().is_empty() {
            let url = reqwest::Url::parse(cfg.whisper_url.trim()).context("неверный whisperUrl")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("whisperUrl должен начинаться с http или https");
            }
        }
        Ok((cfg, w))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_dir;
    #[test]
    fn reads_legacy_and_repairs_only_wrong_keys() {
        let dir = test_dir();
        let fixture = serde_json::json!({"source":"default","systemPrompt":"Мой промпт","claudeArgs":LEGACY_ARGS,"whisperModel":"SMAll-q5_1!","autoSend":false,"fontSize":"loud","bounds":{"x":50,"y":80,"width":640,"height":660}});
        fs::write(dir.join("config.json"), fixture.to_string()).unwrap();
        let (cfg, w) = Config::load(&dir).unwrap();
        assert_eq!(cfg.font_size, 14);
        assert!(w[0].contains("fontSize"));
        assert_eq!(cfg.claude_args, "--model haiku");
        assert_eq!(cfg.bounds.as_ref().unwrap().x, 50);
        assert_eq!(cfg.source, "default");
        assert!(!cfg.auto_send);
        assert_eq!(cfg.system_prompt, "Мой промпт");
        assert_eq!(cfg.whisper_model, "small-q5_1");
        assert!(!cfg.web_search);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn recovers_invalid_json_byte_for_byte() {
        let d = test_dir();
        fs::write(d.join("config.json"), b"{\"source\":1,}").unwrap();
        let (cfg, w) = Config::load(&d).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(w[0].contains("config.bad.json"));
        assert_eq!(
            fs::read(d.join("config.bad.json")).unwrap(),
            b"{\"source\":1,}"
        );
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn clamps_numbers_and_preserves_custom_prompt() {
        let input = serde_json::json!({"opacity":0,"whisperThreads":-1,"fontSize":90,"silenceMs":99,"sensitivity":100,"webSearch":true});
        let (c, _) = Config::default().apply(input.as_object().unwrap()).unwrap();
        assert_eq!(
            (
                c.opacity,
                c.whisper_threads,
                c.font_size,
                c.silence_ms,
                c.sensitivity
            ),
            (0.3, 0, 30, 200, 6.0)
        );
        assert!(c.system_prompt.contains(WITH_WEB));
        let (c, _) = c
            .apply(
                serde_json::json!({"systemPrompt":"свой","whisperThreads":85})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(c.system_prompt, "свой");
        assert_eq!(c.whisper_threads, 32);
    }
}
