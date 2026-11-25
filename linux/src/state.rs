//! Shared settings, menu snapshot and action definitions (aligned with KeyCat 1.1.0).

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
    pub gloves: String,
    pub blush: bool,
    pub whiskers: bool,
    pub mouse_cable: bool,
    pub keyboard_color: String,
    pub opacity: f64,
    pub privacy: bool,
    pub click_through: bool,
    pub sleep_after: f64,
    pub tune: Value,
    pub scale: f64,
    pub sound: bool,
    pub festival: bool,
    pub cup: bool,
    pub second_cat: bool,
    pub pair_mode: String,
    pub coat2: String,
    pub margin_right: i32,
    pub margin_bottom: i32,
    pub monitor_index: Option<i32>,
    pub margin_right_2: Option<i32>,
    pub margin_bottom_2: Option<i32>,
    pub monitor_index_2: Option<i32>,
}

impl Settings {
    pub fn from_store(store: &Store) -> Self {
        let text = |key: &str, fallback: &str| {
            store.get(key).and_then(Value::as_str).unwrap_or(fallback).to_string()
        };
        let number = |key: &str| store.get(key).and_then(Value::as_f64).filter(|v| v.is_finite());
        let int_opt = |key: &str| store.get(key).and_then(Value::as_i64).map(|v| v as i32);

        let coat = text("coat", "black");
        let scale = number("catScale").filter(|v| *v > 0.3).unwrap_or(0.9).clamp(0.4, 2.0);
        let opacity = number("opacity")
            .filter(|v| (0.3..=1.0).contains(v))
            .unwrap_or(1.0);
        let pair_mode = match text("pairMode", "play").as_str() {
            "split" => "split".to_string(),
            _ => "play".to_string(),
        };

        Self {
            style: text("style", "2d"),
            pattern: text(&format!("pattern.{coat}"), "auto"),
            eye: text("eyeColor", "auto"),
            shine: text("eyeShine", "classic"),
            collar: text("collar", "none"),
            gloves: text("gloves", "auto"),
            blush: store.get("blush") == Some(&Value::Bool(true)),
            whiskers: store.get("whiskers") != Some(&Value::Bool(false)),
            mouse_cable: store.get("mouseCable") != Some(&Value::Bool(false)),
            keyboard_color: text("keyboardColor", "cream"),
            opacity,
            privacy: store.get("privacy") == Some(&Value::Bool(true)),
            click_through: store.get("clickThrough") == Some(&Value::Bool(true)),
            sleep_after: number("sleepAfter").unwrap_or(60.0),
            tune: store
                .get("tune")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({})),
            scale,
            sound: store.get("sound") == Some(&Value::Bool(true)),
            festival: store.get("festival") != Some(&Value::Bool(false)),
            cup: store.get("cup") != Some(&Value::Bool(false)),
            second_cat: store.get("secondCat") == Some(&Value::Bool(true)),
            pair_mode,
            coat2: text("cat2.coat", "orange"),
            margin_right: int_opt("marginRight").unwrap_or(DEFAULT_MARGIN_RIGHT),
            margin_bottom: int_opt("marginBottom").unwrap_or(DEFAULT_MARGIN_BOTTOM),
            monitor_index: int_opt("monitorIndex"),
            margin_right_2: int_opt("cat2.marginRight"),
            margin_bottom_2: int_opt("cat2.marginBottom"),
            monitor_index_2: int_opt("cat2.monitorIndex"),
            coat,
        }
    }

    pub fn to_cat_options(&self, is_main: bool, pomodoro: bool) -> Value {
        let role = match (self.second_cat && self.pair_mode == "split", is_main) {
            (false, _) => "both",
            (true, true) => "keys",
            (true, false) => "mouse",
        };
        json!({
            "style": self.style,
            "coat": if is_main { &self.coat } else { &self.coat2 },
            "pattern": if is_main { self.pattern.as_str() } else { "auto" },
            "eye": self.eye,
            "shine": self.shine,
            "blush": self.blush,
            "whiskers": self.whiskers,
            "collar": self.collar,
            "gloves": self.gloves,
            "cable": self.mouse_cable,
            "board": self.keyboard_color,
            "opacity": self.opacity,
            "privacy": self.privacy,
            "sleepAfter": if self.sleep_after > 0.0 { self.sleep_after } else { 1e9 },
            "tune": self.tune,
            "sound": self.sound,
            "festival": self.festival,
            "cup": self.cup,
            "pomodoro": is_main && pomodoro,
            "role": role,
            "persist": is_main,
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
    pub pomodoro: bool,
    pub autostart: bool,
    pub hook_ready: bool,
    pub hook_error: Option<String>,
    pub layer_shell_active: bool,
}

#[derive(Clone, Debug)]
pub enum MenuAction {
    SetValue { key: String, value: Value },
    SetScale(f64),
    TogglePomodoro,
    OpenStats,
    RecordGif,
    ToggleSecondCat,
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

pub const GLOVES: &[(&str, &str)] = &[
    ("跟随毛色", "auto"),
    ("白色", "white"),
    ("黑色", "black"),
    ("粉色", "pink"),
];

pub const COLLARS: &[(&str, &str)] = &[
    ("无", "none"),
    ("红色铃铛", "red"),
    ("蓝色铃铛", "blue"),
    ("粉色铃铛", "pink"),
    ("绿色铃铛", "green"),
];

pub const KEYBOARD_COLORS: &[(&str, &str)] = &[
    ("奶白", "cream"),
    ("黑灰", "dark"),
    ("樱粉", "pink"),
    ("薄荷", "mint"),
];

pub const OPACITIES: &[(&str, f64)] = &[
    ("不透明", 1.0),
    ("85%", 0.85),
    ("70%", 0.7),
    ("55%", 0.55),
];

pub const PAIR_MODES: &[(&str, &str)] = &[
    ("一起玩（各有键盘鼠标，会互相打闹）", "play"),
    ("分工（一只打字，一只管鼠标）", "split"),
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
