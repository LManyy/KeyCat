//! Native GTK3 context menu for right-click on the cat window.
//! Triggered synchronously from the window's own GTK pointer event so Wayland
//! receives a valid input serial.

use gtk::prelude::*;
use serde_json::json;

use crate::state::{
    MenuAction, MenuSnapshot, COATS, COLLARS, EYES, SHINES, SIZES, SLEEP_OPTIONS,
};

fn add_check(menu: &gtk::Menu, label: &str, checked: bool, on_click: impl Fn() + 'static) {
    let item = gtk::CheckMenuItem::with_label(label);
    item.set_active(checked);
    item.connect_activate(move |_| on_click());
    menu.append(&item);
}

fn add_action(menu: &gtk::Menu, label: &str, on_click: impl Fn() + 'static) {
    let item = gtk::MenuItem::with_label(label);
    item.connect_activate(move |_| on_click());
    menu.append(&item);
}

fn add_note(menu: &gtk::Menu, label: &str) {
    let item = gtk::MenuItem::with_label(label);
    item.set_sensitive(false);
    menu.append(&item);
}

fn add_submenu(parent: &gtk::Menu, label: &str, sub: &gtk::Menu) {
    let item = gtk::MenuItem::with_label(label);
    item.set_submenu(Some(sub));
    parent.append(&item);
}

pub fn build_context_menu(snap: &MenuSnapshot, tx: glib::Sender<MenuAction>) -> gtk::Menu {
    let menu = gtk::Menu::new();
    let s = &snap.settings;
    let coat = s.coat.as_str();

    {
        let tx = tx.clone();
        let next_style = if s.style == "3d" { "2d" } else { "3d" };
        add_check(&menu, "立体效果（3D）", s.style == "3d", move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "style".into(),
                value: json!(next_style),
            });
        });
    }

    let coat_menu = gtk::Menu::new();
    for &(label, val) in COATS {
        let tx = tx.clone();
        add_check(&coat_menu, label, coat == val, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "coat".into(),
                value: json!(val),
            });
        });
    }
    add_submenu(&menu, "毛色", &coat_menu);

    if coat == "orange" || coat == "grey" {
        let striped = s.pattern == "tabby" || (s.pattern == "auto" && coat == "orange");
        let pat_menu = gtk::Menu::new();
        for &(label, val, active) in &[("纯色", "solid", !striped), ("虎斑条纹", "tabby", striped)] {
            let tx = tx.clone();
            let key = format!("pattern.{coat}");
            add_check(&pat_menu, label, active, move || {
                let _ = tx.send(MenuAction::SetValue {
                    key: key.clone(),
                    value: json!(val),
                });
            });
        }
        add_submenu(&menu, "花纹", &pat_menu);
    }

    let look_menu = gtk::Menu::new();
    let eye_menu = gtk::Menu::new();
    for &(label, val) in EYES {
        let tx = tx.clone();
        add_check(&eye_menu, label, s.eye == val, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "eyeColor".into(),
                value: json!(val),
            });
        });
    }
    add_submenu(&look_menu, "眼睛颜色", &eye_menu);

    let shine_menu = gtk::Menu::new();
    for &(label, val) in SHINES {
        let tx = tx.clone();
        add_check(&shine_menu, label, s.shine == val, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "eyeShine".into(),
                value: json!(val),
            });
        });
    }
    add_submenu(&look_menu, "眼睛高光", &shine_menu);

    let collar_menu = gtk::Menu::new();
    for &(label, val) in COLLARS {
        let tx = tx.clone();
        add_check(&collar_menu, label, s.collar == val, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "collar".into(),
                value: json!(val),
            });
        });
    }
    add_submenu(&look_menu, "项圈", &collar_menu);

    {
        let tx = tx.clone();
        let next = !s.blush;
        add_check(&look_menu, "腮红", s.blush, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "blush".into(),
                value: json!(next),
            });
        });
    }
    {
        let tx = tx.clone();
        let next = !s.whiskers;
        add_check(&look_menu, "胡须", s.whiskers, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "whiskers".into(),
                value: json!(next),
            });
        });
    }
    add_submenu(&menu, "外观", &look_menu);

    let size_menu = gtk::Menu::new();
    for &(label, val) in SIZES {
        let tx = tx.clone();
        add_check(&size_menu, label, (s.scale - val).abs() < 0.01, move || {
            let _ = tx.send(MenuAction::SetScale(val));
        });
    }
    add_submenu(&menu, "大小", &size_menu);

    let sleep_menu = gtk::Menu::new();
    for &(label, val) in SLEEP_OPTIONS {
        let tx = tx.clone();
        add_check(&sleep_menu, label, (s.sleep_after - val).abs() < 0.01, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "sleepAfter".into(),
                value: json!(val),
            });
        });
    }
    add_submenu(&menu, "多久睡着", &sleep_menu);

    {
        let tx = tx.clone();
        let next = !s.privacy;
        add_check(&menu, "隐私模式（不显示键名）", s.privacy, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "privacy".into(),
                value: json!(next),
            });
        });
    }
    {
        let tx = tx.clone();
        let next = !s.click_through;
        add_check(&menu, "鼠标穿透（点击直接落到下层窗口）", s.click_through, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: "clickThrough".into(),
                value: json!(next),
            });
        });
    }
    {
        let tx = tx.clone();
        add_action(&menu, "调节反应…", move || {
            let _ = tx.send(MenuAction::OpenSettings);
        });
    }

    menu.append(&gtk::SeparatorMenuItem::new());

    {
        let tx = tx.clone();
        add_check(&menu, "开机自启", snap.autostart, move || {
            let _ = tx.send(MenuAction::ToggleAutostart);
        });
    }
    {
        let tx = tx.clone();
        add_action(&menu, "回到右下角", move || {
            let _ = tx.send(MenuAction::ResetHome);
        });
    }
    {
        let tx = tx.clone();
        add_action(&menu, if snap.visible { "隐藏" } else { "显示" }, move || {
            let _ = tx.send(MenuAction::ToggleVisibility);
        });
    }

    if !snap.hook_ready {
        menu.append(&gtk::SeparatorMenuItem::new());
        add_note(
            &menu,
            snap.hook_error
                .as_deref()
                .unwrap_or("键盘监听未启动（请加入 input 组）"),
        );
    }

    menu.append(&gtk::SeparatorMenuItem::new());
    {
        let tx = tx.clone();
        add_action(&menu, "退出 KeyCat", move || {
            let _ = tx.send(MenuAction::Quit);
        });
    }

    menu.show_all();
    menu
}
