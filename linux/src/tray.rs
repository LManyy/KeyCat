//! KDE Plasma native system tray via StatusNotifierItem (`ksni`), aligned with KeyCat 1.1.0 menu.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Handle, Icon, ToolTip, Tray, TrayService};
use serde_json::{json, Value};

use crate::state::{
    MenuAction, MenuSnapshot, COATS, COLLARS, EYES, GLOVES, KEYBOARD_COLORS, OPACITIES,
    PAIR_MODES, SHINES, SIZES, SLEEP_OPTIONS,
};

struct KeyCatTray {
    snapshot: Arc<Mutex<MenuSnapshot>>,
    tx: glib::Sender<MenuAction>,
    icon: Vec<Icon>,
}

fn load_tray_icon() -> Vec<Icon> {
    let bytes = include_bytes!("../icons/tray.png");
    let mut decoder = png::Decoder::new(Cursor::new(bytes.as_slice()));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let Ok(mut reader) = decoder.read_info() else {
        return Vec::new();
    };
    let mut buf = vec![0; reader.output_buffer_size()];
    let Ok(info) = reader.next_frame(&mut buf) else {
        return Vec::new();
    };
    let raw = &buf[..info.buffer_size()];
    let mut argb = Vec::with_capacity((info.width * info.height * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => {
            for px in raw.as_chunks::<4>().0 {
                argb.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
            }
        }
        png::ColorType::Rgb => {
            for px in raw.as_chunks::<3>().0 {
                argb.extend_from_slice(&[255, px[0], px[1], px[2]]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in raw.as_chunks::<2>().0 {
                argb.extend_from_slice(&[px[1], px[0], px[0], px[0]]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in raw {
                argb.extend_from_slice(&[255, g, g, g]);
            }
        }
        _ => return Vec::new(),
    }
    vec![Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: argb,
    }]
}

impl KeyCatTray {
    fn emit(&self, action: MenuAction) {
        let _ = self.tx.send(action);
    }
}

fn check_item(
    label: &str,
    checked: bool,
    on_activate: impl Fn(&mut KeyCatTray) + Send + Sync + 'static,
) -> MenuItem<KeyCatTray> {
    CheckmarkItem {
        label: label.into(),
        checked,
        activate: Box::new(on_activate),
        ..Default::default()
    }
    .into()
}

fn action_item(label: &str, action: MenuAction) -> MenuItem<KeyCatTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |tray: &mut KeyCatTray| tray.emit(action.clone())),
        ..Default::default()
    }
    .into()
}

fn toggle_item(label: &str, key: &'static str, current: bool) -> MenuItem<KeyCatTray> {
    let next = !current;
    check_item(label, current, move |tray| {
        tray.emit(MenuAction::SetValue {
            key: key.into(),
            value: json!(next),
        });
    })
}

fn choice_str(
    title: &str,
    key: &str,
    current: &str,
    options: &[(&str, &'static str)],
) -> MenuItem<KeyCatTray> {
    let submenu = options
        .iter()
        .map(|&(label, val)| {
            let key = key.to_string();
            check_item(label, current == val, move |tray| {
                tray.emit(MenuAction::SetValue {
                    key: key.clone(),
                    value: json!(val),
                });
            })
        })
        .collect();
    SubMenu {
        label: title.into(),
        submenu,
        ..Default::default()
    }
    .into()
}

fn choice_f64(
    title: &str,
    key: &'static str,
    current: f64,
    options: &[(&str, f64)],
) -> MenuItem<KeyCatTray> {
    let submenu = options
        .iter()
        .map(|&(label, val)| {
            check_item(label, (current - val).abs() < 0.01, move |tray| {
                tray.emit(MenuAction::SetValue {
                    key: key.into(),
                    value: Value::from(val),
                });
            })
        })
        .collect();
    SubMenu {
        label: title.into(),
        submenu,
        ..Default::default()
    }
    .into()
}

impl Tray for KeyCatTray {
    fn id(&self) -> String {
        "com.ellobok.keycat".into()
    }

    fn title(&self) -> String {
        "KeyCat".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "KeyCat".into(),
            description: "桌面打字猫".into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.emit(MenuAction::ToggleVisibility);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let snap = self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let s = &snap.settings;
        let coat = s.coat.as_str();

        let mut items: Vec<MenuItem<Self>> = vec![
            check_item(
                if snap.pomodoro { "结束番茄钟" } else { "开始番茄钟" },
                snap.pomodoro,
                |tray| tray.emit(MenuAction::TogglePomodoro),
            ),
            action_item("今日小结…", MenuAction::OpenStats),
            action_item("拍一张动图（3 秒）", MenuAction::RecordGif),
            MenuItem::Separator,
        ];

        // 外观
        let mut look: Vec<MenuItem<Self>> = vec![choice_str("毛色", "coat", coat, COATS)];
        if coat == "orange" || coat == "grey" {
            let striped = s.pattern == "tabby" || (s.pattern == "auto" && coat == "orange");
            let current = if striped { "tabby" } else { "solid" };
            look.push(choice_str(
                "花纹",
                &format!("pattern.{coat}"),
                current,
                &[("纯色", "solid"), ("虎斑条纹", "tabby")],
            ));
        }
        let size_items = SIZES
            .iter()
            .map(|&(label, val)| {
                check_item(label, (s.scale - val).abs() < 0.01, move |tray| {
                    tray.emit(MenuAction::SetScale(val));
                })
            })
            .collect();
        look.push(
            SubMenu {
                label: "大小".into(),
                submenu: size_items,
                ..Default::default()
            }
            .into(),
        );
        let next_style = if s.style == "3d" { "2d" } else { "3d" };
        look.push(check_item("立体效果（3D）", s.style == "3d", move |tray| {
            tray.emit(MenuAction::SetValue {
                key: "style".into(),
                value: json!(next_style),
            });
        }));
        look.push(MenuItem::Separator);
        look.push(choice_str("眼睛颜色", "eyeColor", &s.eye, EYES));
        look.push(choice_str("眼睛高光", "eyeShine", &s.shine, SHINES));
        look.push(choice_str("手套颜色", "gloves", &s.gloves, GLOVES));
        look.push(choice_str("项圈", "collar", &s.collar, COLLARS));
        look.push(toggle_item("腮红", "blush", s.blush));
        look.push(toggle_item("胡须", "whiskers", s.whiskers));
        look.push(toggle_item("节日装扮", "festival", s.festival));
        items.push(
            SubMenu {
                label: "外观".into(),
                submenu: look,
                ..Default::default()
            }
            .into(),
        );

        // 桌面
        items.push(
            SubMenu {
                label: "桌面".into(),
                submenu: vec![
                    choice_str("键盘颜色", "keyboardColor", &s.keyboard_color, KEYBOARD_COLORS),
                    toggle_item("鼠标线", "mouseCable", s.mouse_cable),
                    toggle_item("桌上的杯子（它会推下去）", "cup", s.cup),
                    choice_f64("透明度", "opacity", s.opacity, OPACITIES),
                ],
                ..Default::default()
            }
            .into(),
        );

        // 习性
        items.push(
            SubMenu {
                label: "习性".into(),
                submenu: vec![
                    choice_f64("多久睡着", "sleepAfter", s.sleep_after, SLEEP_OPTIONS),
                    toggle_item("声音（摸它时呼噜）", "sound", s.sound),
                    action_item("调节反应…", MenuAction::OpenSettings),
                ],
                ..Default::default()
            }
            .into(),
        );

        // 第二只猫
        items.push(
            SubMenu {
                label: "第二只猫".into(),
                submenu: vec![
                    check_item("陪着它", s.second_cat, |tray| {
                        tray.emit(MenuAction::ToggleSecondCat);
                    }),
                    choice_str("它的毛色", "cat2.coat", &s.coat2, COATS),
                    choice_str("怎么相处", "pairMode", &s.pair_mode, PAIR_MODES),
                ],
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(toggle_item("隐私模式（不显示键名）", "privacy", s.privacy));
        items.push(toggle_item(
            "鼠标穿透（点击直接落到下层窗口）",
            "clickThrough",
            s.click_through,
        ));
        items.push(action_item("回到右下角", MenuAction::ResetHome));
        items.push(action_item(
            if snap.visible { "隐藏" } else { "显示" },
            MenuAction::ToggleVisibility,
        ));

        items.push(MenuItem::Separator);
        items.push(check_item("开机自启", snap.autostart, |tray| {
            tray.emit(MenuAction::ToggleAutostart);
        }));

        if !snap.hook_ready {
            items.push(MenuItem::Separator);
            items.push(
                StandardItem {
                    label: snap
                        .hook_error
                        .as_deref()
                        .unwrap_or("键盘监听未启动（请加入 input 组）")
                        .into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        if !snap.layer_shell_active {
            items.push(
                StandardItem {
                    label: "提示：当前未启用 Wayland Layer-Shell".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(action_item("退出 KeyCat", MenuAction::Quit));
        items
    }
}

pub struct TrayController {
    snapshot: Arc<Mutex<MenuSnapshot>>,
    handle: Handle<KeyCatTray>,
}

impl TrayController {
    pub fn start(initial: MenuSnapshot, tx: glib::Sender<MenuAction>) -> Self {
        let snapshot = Arc::new(Mutex::new(initial));
        let tray = KeyCatTray {
            snapshot: snapshot.clone(),
            tx,
            icon: load_tray_icon(),
        };
        let service = TrayService::new(tray);
        let handle = service.handle();
        service.spawn();
        Self { snapshot, handle }
    }

    pub fn update(&self, next: MenuSnapshot) {
        if let Ok(mut guard) = self.snapshot.lock() {
            *guard = next;
        }
        self.handle.update(|_| {});
    }
}
