//! Application localisation primitives.
//!
//! Translation resources intentionally live beside the application binary.  The
//! small Fluent-like parser here supports the `key = value` messages used by the
//! UI and `{name}` substitutions, while keeping the application independent of
//! a runtime locale installation.

#[cfg(target_os = "macos")]
use std::process::Command;
use std::{
    collections::HashMap,
    env,
    sync::atomic::{AtomicU8, Ordering},
};

const EN: &str = include_str!("../locales/en.ftl");
const ZH_HANS: &str = include_str!("../locales/zh-Hans.ftl");
static ACTIVE_LOCALE: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguagePreference {
    #[default]
    System,
    English,
    SimplifiedChinese,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    En,
    ZhHans,
}

impl LanguagePreference {
    pub fn locale(self) -> Locale {
        match self {
            Self::System => system_locale(),
            Self::English => Locale::En,
            Self::SimplifiedChinese => Locale::ZhHans,
        }
    }
}

pub fn set_active(locale: Locale) {
    ACTIVE_LOCALE.store(
        if locale == Locale::ZhHans { 1 } else { 0 },
        Ordering::Relaxed,
    );
}

/// Translate legacy UI labels while the call sites are migrated to keyed messages.
/// Keeping this small table here lets the settings and command chrome respond to a
/// language change immediately without making every GPUI helper locale-aware first.
pub fn legacy_text(text: &str) -> String {
    if ACTIVE_LOCALE.load(Ordering::Relaxed) == 0 {
        return text.to_owned();
    }
    match text {
        "Startup" => "启动",
        "Show and hide" => "显示与隐藏",
        "New note" => "新建笔记",
        "Note window" => "笔记窗口",
        "Appearance" => "外观",
        "Language" => "语言",
        "General" => "通用",
        "Editor" => "编辑器",
        "Files" => "文件",
        "Markdown" => "Markdown",
        "About" => "关于",
        "Auto" => "自动",
        "Light" => "浅色",
        "Dark" => "深色",
        "System" => "跟随系统",
        "Default" => "默认",
        "Editing" => "编辑",
        "Images" => "图片",
        "Show on open" => "打开时显示",
        "Last Note" => "上次笔记",
        "Launch at login" => "登录时启动",
        "Keep above other windows" => "保持在其他窗口上方",
        "Hide when another app is used" => "使用其他应用时隐藏",
        "Grow with the note" => "随笔记内容增长",
        "Show on all desktops" => "显示在所有桌面",
        "Open on the display with the pointer" => "在指针所在屏幕打开",
        "No matching actions" => "没有匹配的操作",
        "No matching notes" => "没有匹配的笔记",
        "Edited" => "编辑于",
        "Current" => "当前",
        "No file yet" => "尚未保存文件",
        "Font" => "字体",
        "Line height" => "行高",
        "Line width" => "行宽",
        "Tab key" => "Tab 键",
        "Tight" => "紧凑",
        "Normal" => "正常",
        "Relaxed" => "宽松",
        "Narrow" => "窄",
        "Full" => "全宽",
        "2 Spaces" => "2 个空格",
        "4 Spaces" => "4 个空格",
        "Serif" => "衬线",
        "Rounded" => "圆体",
        "Mono" => "等宽",
        "Settings" => "设置",
        "Save Now" => "立即保存",
        "Browse Notes" => "浏览笔记",
        "Show Folder in Finder" => "在 Finder 中显示文件夹",
        "Toggle Task" => "切换任务",
        "Choose Code Language" => "选择代码语言",
        "Copy Code Block" => "复制代码块",
        "Copy Link" => "复制链接",
        "Open Link" => "打开链接",
        "Remove Link" => "移除链接",
        "Add Row Above" => "在上方添加行",
        "Add Row Below" => "在下方添加行",
        "Add Column Left" => "在左侧添加列",
        "Add Column Right" => "在右侧添加列",
        "Delete Row" => "删除行",
        "Delete Column" => "删除列",
        "Delete Table" => "删除表格",
        "Align Column Left" => "列左对齐",
        "Align Column Center" => "列居中对齐",
        "Align Column Right" => "列右对齐",
        "Switch Folder…" => "切换文件夹…",
        "Open Folder…" => "打开文件夹…",
        "Hide Window" => "隐藏窗口",
        "Show Window" => "显示窗口",
        "Quit" => "退出",
        "Undo" => "撤销",
        "Redo" => "重做",
        "Cut" => "剪切",
        "Copy" => "复制",
        "Paste" => "粘贴",
        "Select All" => "全选",
        "Find" => "查找",
        "New Note" => "新建笔记",
        "Delete Note" => "删除笔记",
        "Rename Note" => "重命名笔记",
        "Save" => "保存",
        "Close" => "关闭",
        "Check for Updates…" => "检查更新…",
        "Report an Issue…" => "报告问题…",
        _ => text,
    }
    .to_owned()
}

/// Resolve the user's preferred system language. Unknown languages use English.
pub fn system_locale() -> Locale {
    let mut values = Vec::new();
    #[cfg(target_os = "macos")]
    if let Ok(output) = Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        values.extend(extract_language_tags(&String::from_utf8_lossy(
            &output.stdout,
        )));
    }
    if values.is_empty() {
        if let Some(value) = env::var_os("LC_ALL").or_else(|| env::var_os("LANG")) {
            values.push(value.to_string_lossy().into_owned());
        }
    }
    for value in values {
        let value = value.to_ascii_lowercase().replace('_', "-");
        if value.starts_with("zh-hant") || value.starts_with("zh-tw") || value.starts_with("zh-hk")
        {
            return Locale::En;
        }
        if value.starts_with("zh") || value.starts_with("chinese") {
            return Locale::ZhHans;
        }
        if value.starts_with("en") {
            return Locale::En;
        }
    }
    Locale::En
}

fn extract_language_tags(value: &str) -> Vec<String> {
    value
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter(|part| {
            let p = part.to_ascii_lowercase();
            p == "zh" || p.starts_with("zh-") || p == "en" || p.starts_with("en-")
        })
        .map(str::to_owned)
        .collect()
}

#[derive(Clone, Debug)]
pub struct Translator {
    #[allow(dead_code)]
    pub locale: Locale,
    #[allow(dead_code)]
    pub fallback: Locale,
    messages: HashMap<String, String>,
    fallback_messages: HashMap<String, String>,
}

impl Translator {
    pub fn new(locale: Locale) -> Self {
        let fallback = Locale::En;
        let fallback_messages = parse_messages(EN);
        let messages = if locale == Locale::En {
            fallback_messages.clone()
        } else {
            parse_messages(ZH_HANS)
        };
        Self {
            locale,
            fallback,
            messages,
            fallback_messages,
        }
    }

    #[allow(dead_code)]
    pub fn for_preference(preference: LanguagePreference) -> Self {
        Self::new(preference.locale())
    }

    pub fn text(&self, key: &str) -> String {
        self.messages
            .get(key)
            .or_else(|| self.fallback_messages.get(key))
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }

    pub fn text_with(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut value = self.text(key);
        for (name, replacement) in args {
            value = value.replace(&format!("{{{name}}}"), replacement);
        }
        value
    }

    #[allow(dead_code)]
    pub fn validate(&self, key: &str, args: &[&str]) -> bool {
        let value = self.text(key);
        args.iter()
            .all(|name| value.contains(&format!("{{{name}}}")))
    }
}

fn parse_messages(source: &str) -> HashMap<String, String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn translates_and_falls_back() {
        let zh = Translator::new(Locale::ZhHans);
        assert_eq!(zh.text("language"), "语言");
        assert_eq!(zh.text("missing.key"), "missing.key");
        assert_eq!(zh.text_with("greeting", &[("name", "Ada")]), "你好，Ada");
    }
    #[test]
    fn preference_round_trips() {
        let json = serde_json::to_string(&LanguagePreference::SimplifiedChinese).unwrap();
        assert_eq!(json, "\"simplified_chinese\"");
        assert_eq!(
            serde_json::from_str::<LanguagePreference>(&json).unwrap(),
            LanguagePreference::SimplifiedChinese
        );
    }

    #[test]
    fn legacy_labels_follow_active_locale() {
        set_active(Locale::ZhHans);
        assert_eq!(legacy_text("Settings"), "设置");
        assert_eq!(legacy_text("No matching notes"), "没有匹配的笔记");
        set_active(Locale::En);
        assert_eq!(legacy_text("No matching notes"), "No matching notes");
    }

    #[test]
    fn locale_resources_cover_the_base_catalog() {
        let english = parse_messages(EN);
        let chinese = parse_messages(ZH_HANS);
        let missing: Vec<_> = english
            .keys()
            .filter(|key| !chinese.contains_key(*key))
            .collect();
        assert!(missing.is_empty(), "zh-Hans is missing keys: {missing:?}");
    }

    #[test]
    fn system_language_uses_order_and_rejects_traditional_chinese() {
        assert_eq!(
            extract_language_tags("( en-US, zh-Hans )"),
            vec!["en-US", "zh-Hans"]
        );
        assert_eq!(
            extract_language_tags("( zh-Hant, en-US )"),
            vec!["zh-Hant", "en-US"]
        );
    }
}
