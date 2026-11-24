//! Shared settings, menu snapshot and action definitions.

use serde_json::{json, Value};

use crate::store::Store;

pub const DESIGN: (f64, f64) = (360.0, 368.0);
pub const DEFAULT_MARGIN_RIGHT: i32 = 20;
pub const DEFAULT_MARGIN_BOTTOM: i32 = 0;

#[derive(Clone, Debug)]
pub struct Settings {
    pub style: String,
    pub coat: String,
    pub pattern: String,
    pub eye: String,
    pub shine: String,
    pub collar: String,
    pub blush: bool,
    pub whiskers: bool,
    pub privacy: bool,
    pub click_through: bool,
    pub sleep_after: f64,
    pub tune: Value,
    pub scale: f64,
    pub margin_right: i32,
    pub margin_bottom: i32,
    pub monitor_index: Option<i32>,
}

impl Settings {
    pub fn from_store(store: &Store) -> Self {
        let text = |key: &str, fallback: &str| {
            store.get(key).and_then(Value::as_str).unwrap_or(fallback).to_string()
        };
        let number = |key: &str| store.get(key).and_then(Value::as_f64).filter(|v| v.is_finite());
        let coat = text("coat", "black");
        let scale = number("catScale").filter(|v| *v > 0.3).unwrap_or(0.9).clamp(0.4, 2.0);
        let margin_right = store
            .get("marginRight")
            .and_then(Value::as_i64)
            .map(|v| v as i32)
            .unwrap_or(DEFAULT_MARGIN_RIGHT);
        let margin_bottom = store
            .get("marginBottom")
            .and_then(Value::as_i64)
            .map(|v| v as i32)
            .unwrap_or(DEFAULT_MARGIN_BOTTOM);
        let monitor_index = store
            .get("monitorIndex")
            .and_then(Value::as_i64)
            .map(|v| v as i32);

        Self {
            style: text("style", "2d"),
            pattern: text(&format!("pattern.{coat}"), "auto"),
            eye: text("eyeColor", "auto"),
            shine: text("eyeShine", "classic"),
            collar: text("collar", "none"),
            blush: store.get("blush") == Some(&Value::Bool(true)),
            whiskers: store.get("whiskers") != Some(&Value::Bool(false)),
            privacy: store.get("privacy") == Some(&Value::Bool(true)),
            click_through: store.get("clickThrough") == Some(&Value::Bool(true)),
            sleep_after: number("sleepAfter").unwrap_or(60.0),
            tune: store
                .get("tune")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({})),
            scale,
            margin_right,
            margin_bottom,
            monitor_index,
            coat,
        }
    }

    pub fn to_cat_options(&self) -> Value {
        json!({
            "style": self.style,
            "coat": self.coat,
            "pattern": self.pattern,
            "eye": self.eye,
            "shine": self.shine,
            "blush": self.blush,
            "whiskers": self.whiskers,
            "collar": self.collar,
            "privacy": self.privacy,
            "sleepAfter": if self.sleep_after > 0.0 { self.sleep_after } else { 1e9 },
            "tune": self.tune,
        })
    }

    pub fn window_size(&self) -> (i32, i32) {
        ((DESIGN.0 * self.scale).round() as i32, (DESIGN.1 * self.scale).round() as i32)
    }
}

#[derive(Clone, Debug)]
pub struct MenuSnapshot {
    pub settings: Settings,
    pub visible: bool,
    pub autostart: bool,
    pub hook_ready: bool,
    pub hook_error: Option<String>,
    pub layer_shell_active: bool,
}

#[derive(Clone, Debug)]
pub enum MenuAction {
    SetValue { key: String, value: Value },
    SetScale(f64),
    OpenSettings,
    ToggleAutostart,
    ResetHome,
    ToggleVisibility,
    Quit,
}

pub const COATS: &[(&str, &str)] = &[
    ("黑猫", "black"),
    ("奶牛猫", "tuxedo"),
    ("橘猫", "orange"),
    ("蓝灰猫", "grey"),
    ("白猫", "white"),
    ("三花猫", "calico"),
    ("暹罗猫", "siamese"),
    ("布偶猫", "ragdoll"),
    ("德文卷毛猫", "devon"),
    ("美短银虎斑", "silver"),
    ("狸花猫", "lihua"),
];

pub const EYES: &[(&str, &str)] = &[
    ("跟随毛色", "auto"),
    ("金色", "gold"),
    ("绿色", "green"),
    ("蓝色", "blue"),
    ("琥珀色", "amber"),
    ("鸳鸯眼（一蓝一金）", "odd"),
];

pub const SHINES: &[(&str, &str)] = &[
    ("无", "none"),
    ("经典", "classic"),
    ("闪亮", "sparkle"),
];

pub const COLLARS: &[(&str, &str)] = &[
    ("无", "none"),
    ("红色铃铛", "red"),
    ("蓝色铃铛", "blue"),
    ("粉色铃铛", "pink"),
    ("绿色铃铛", "green"),
];

pub const SIZES: &[(&str, f64)] = &[
    ("特小", 0.5),
    ("小", 0.7),
    ("中", 0.9),
    ("大", 1.1),
    ("特大", 1.35),
];

pub const SLEEP_OPTIONS: &[(&str, f64)] = &[
    ("30 秒", 30.0),
    ("1 分钟", 60.0),
    ("3 分钟", 180.0),
    ("10 分钟", 600.0),
    ("不睡", 0.0),
];
