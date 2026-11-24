//! KDE Plasma native system tray via StatusNotifierItem (`ksni`).

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Handle, Icon, ToolTip, Tray, TrayService};
use serde_json::json;

use crate::state::{
    MenuAction, MenuSnapshot, COATS, COLLARS, EYES, SHINES, SIZES, SLEEP_OPTIONS,
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

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        let next_style = if s.style == "3d" { "2d" } else { "3d" };
        items.push(
            CheckmarkItem {
                label: "立体效果（3D）".into(),
                checked: s.style == "3d",
                activate: Box::new(move |tray: &mut Self| {
                    tray.emit(MenuAction::SetValue {
                        key: "style".into(),
                        value: json!(next_style),
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        let coat_items = COATS
            .iter()
            .map(|&(label, val)| {
                let checked = coat == val;
                CheckmarkItem {
                    label: label.into(),
                    checked,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetValue {
                            key: "coat".into(),
                            value: json!(val),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "毛色".into(),
                submenu: coat_items,
                ..Default::default()
            }
            .into(),
        );

        if coat == "orange" || coat == "grey" {
            let striped = s.pattern == "tabby" || (s.pattern == "auto" && coat == "orange");
            let key = format!("pattern.{coat}");
            let key_solid = key.clone();
            let key_tabby = key;
            items.push(
                SubMenu {
                    label: "花纹".into(),
                    submenu: vec![
                        CheckmarkItem {
                            label: "纯色".into(),
                            checked: !striped,
                            activate: Box::new(move |tray: &mut Self| {
                                tray.emit(MenuAction::SetValue {
                                    key: key_solid.clone(),
                                    value: json!("solid"),
                                });
                            }),
                            ..Default::default()
                        }
                        .into(),
                        CheckmarkItem {
                            label: "虎斑条纹".into(),
                            checked: striped,
                            activate: Box::new(move |tray: &mut Self| {
                                tray.emit(MenuAction::SetValue {
                                    key: key_tabby.clone(),
                                    value: json!("tabby"),
                                });
                            }),
                            ..Default::default()
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }

        let eye_items = EYES
            .iter()
            .map(|&(label, val)| {
                CheckmarkItem {
                    label: label.into(),
                    checked: s.eye == val,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetValue {
                            key: "eyeColor".into(),
                            value: json!(val),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        let shine_items = SHINES
            .iter()
            .map(|&(label, val)| {
                CheckmarkItem {
                    label: label.into(),
                    checked: s.shine == val,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetValue {
                            key: "eyeShine".into(),
                            value: json!(val),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        let collar_items = COLLARS
            .iter()
            .map(|&(label, val)| {
                CheckmarkItem {
                    label: label.into(),
                    checked: s.collar == val,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetValue {
                            key: "collar".into(),
                            value: json!(val),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();

        let next_blush = !s.blush;
        let next_whiskers = !s.whiskers;
        items.push(
            SubMenu {
                label: "外观".into(),
                submenu: vec![
                    SubMenu {
                        label: "眼睛颜色".into(),
                        submenu: eye_items,
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: "眼睛高光".into(),
                        submenu: shine_items,
                        ..Default::default()
                    }
                    .into(),
                    SubMenu {
                        label: "项圈".into(),
                        submenu: collar_items,
                        ..Default::default()
                    }
                    .into(),
                    CheckmarkItem {
                        label: "腮红".into(),
                        checked: s.blush,
                        activate: Box::new(move |tray: &mut Self| {
                            tray.emit(MenuAction::SetValue {
                                key: "blush".into(),
                                value: json!(next_blush),
                            });
                        }),
                        ..Default::default()
                    }
                    .into(),
                    CheckmarkItem {
                        label: "胡须".into(),
                        checked: s.whiskers,
                        activate: Box::new(move |tray: &mut Self| {
                            tray.emit(MenuAction::SetValue {
                                key: "whiskers".into(),
                                value: json!(next_whiskers),
                            });
                        }),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
        );

        let size_items = SIZES
            .iter()
            .map(|&(label, val)| {
                CheckmarkItem {
                    label: label.into(),
                    checked: (s.scale - val).abs() < 0.01,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetScale(val));
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "大小".into(),
                submenu: size_items,
                ..Default::default()
            }
            .into(),
        );

        let sleep_items = SLEEP_OPTIONS
            .iter()
            .map(|&(label, val)| {
                CheckmarkItem {
                    label: label.into(),
                    checked: (s.sleep_after - val).abs() < 0.01,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.emit(MenuAction::SetValue {
                            key: "sleepAfter".into(),
                            value: json!(val),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "多久睡着".into(),
                submenu: sleep_items,
                ..Default::default()
            }
            .into(),
        );

        let next_privacy = !s.privacy;
        items.push(
            CheckmarkItem {
                label: "隐私模式（不显示键名）".into(),
                checked: s.privacy,
                activate: Box::new(move |tray: &mut Self| {
                    tray.emit(MenuAction::SetValue {
                        key: "privacy".into(),
                        value: json!(next_privacy),
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        let next_ct = !s.click_through;
        items.push(
            CheckmarkItem {
                label: "鼠标穿透（点击直接落到下层窗口）".into(),
                checked: s.click_through,
                activate: Box::new(move |tray: &mut Self| {
                    tray.emit(MenuAction::SetValue {
                        key: "clickThrough".into(),
                        value: json!(next_ct),
                    });
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            StandardItem {
                label: "调节反应…".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(MenuAction::OpenSettings)),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            CheckmarkItem {
                label: "开机自启".into(),
                checked: snap.autostart,
                activate: Box::new(|tray: &mut Self| tray.emit(MenuAction::ToggleAutostart)),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            StandardItem {
                label: "回到右下角".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(MenuAction::ResetHome)),
                ..Default::default()
            }
            .into(),
        );

        items.push(
            StandardItem {
                label: if snap.visible { "隐藏" } else { "显示" }.into(),
                activate: Box::new(|tray: &mut Self| tray.emit(MenuAction::ToggleVisibility)),
                ..Default::default()
            }
            .into(),
        );

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

        items.push(
            StandardItem {
                label: "退出 KeyCat".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(MenuAction::Quit)),
                ..Default::default()
            }
            .into(),
        );

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
