//! Native GTK3 context menu for right-click on a cat window, aligned with KeyCat 1.1.0 menu.

use gtk::prelude::*;
use serde_json::{json, Value};

use crate::state::{
    MenuAction, MenuSnapshot, COATS, COLLARS, EYES, GLOVES, KEYBOARD_COLORS, OPACITIES,
    PAIR_MODES, SHINES, SIZES, SLEEP_OPTIONS,
};

fn add_check(menu: &gtk::Menu, label: &str, checked: bool, on_click: impl Fn() + 'static) {
    let item = gtk::CheckMenuItem::with_label(label);
    item.set_active(checked);
    item.connect_activate(move |_| on_click());
    menu.append(&item);
}

fn add_action(menu: &gtk::Menu, label: &str, tx: &glib::Sender<MenuAction>, action: MenuAction) {
    let item = gtk::MenuItem::with_label(label);
    let tx = tx.clone();
    item.connect_activate(move |_| {
        let _ = tx.send(action.clone());
    });
    menu.append(&item);
}

fn add_toggle(
    menu: &gtk::Menu,
    label: &str,
    key: &'static str,
    current: bool,
    tx: &glib::Sender<MenuAction>,
) {
    let tx = tx.clone();
    let next = !current;
    add_check(menu, label, current, move || {
        let _ = tx.send(MenuAction::SetValue {
            key: key.into(),
            value: json!(next),
        });
    });
}

fn add_submenu(parent: &gtk::Menu, label: &str, sub: &gtk::Menu) {
    let item = gtk::MenuItem::with_label(label);
    item.set_submenu(Some(sub));
    parent.append(&item);
}

fn add_choice_str(
    parent: &gtk::Menu,
    title: &str,
    key: &str,
    current: &str,
    options: &[(&str, &'static str)],
    tx: &glib::Sender<MenuAction>,
) {
    let sub = gtk::Menu::new();
    for &(label, val) in options {
        let tx = tx.clone();
        let key = key.to_string();
        add_check(&sub, label, current == val, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: key.clone(),
                value: json!(val),
            });
        });
    }
    add_submenu(parent, title, &sub);
}

fn add_choice_f64(
    parent: &gtk::Menu,
    title: &str,
    key: &'static str,
    current: f64,
    options: &[(&str, f64)],
    tx: &glib::Sender<MenuAction>,
) {
    let sub = gtk::Menu::new();
    for &(label, val) in options {
        let tx = tx.clone();
        add_check(&sub, label, (current - val).abs() < 0.01, move || {
            let _ = tx.send(MenuAction::SetValue {
                key: key.into(),
                value: Value::from(val),
            });
        });
    }
    add_submenu(parent, title, &sub);
}

pub fn build_context_menu(snap: &MenuSnapshot, tx: glib::Sender<MenuAction>) -> gtk::Menu {
    let menu = gtk::Menu::new();
    let s = &snap.settings;
    let coat = s.coat.as_str();

    {
        let tx_c = tx.clone();
        add_check(
            &menu,
            if snap.pomodoro { "结束番茄钟" } else { "开始番茄钟" },
            snap.pomodoro,
            move || {
                let _ = tx_c.send(MenuAction::TogglePomodoro);
            },
        );
    }
    add_action(&menu, "今日小结…", &tx, MenuAction::OpenStats);
    add_action(&menu, "拍一张动图（3 秒）", &tx, MenuAction::RecordGif);
    menu.append(&gtk::SeparatorMenuItem::new());

    // 外观
    let look = gtk::Menu::new();
    add_choice_str(&look, "毛色", "coat", coat, COATS, &tx);
    if coat == "orange" || coat == "grey" {
        let striped = s.pattern == "tabby" || (s.pattern == "auto" && coat == "orange");
        let current = if striped { "tabby" } else { "solid" };
        add_choice_str(
            &look,
            "花纹",
            &format!("pattern.{coat}"),
            current,
            &[("纯色", "solid"), ("虎斑条纹", "tabby")],
            &tx,
        );
    }
    let size_menu = gtk::Menu::new();
    for &(label, val) in SIZES {
        let tx_c = tx.clone();
        add_check(&size_menu, label, (s.scale - val).abs() < 0.01, move || {
            let _ = tx_c.send(MenuAction::SetScale(val));
        });
    }
    add_submenu(&look, "大小", &size_menu);
    {
        let tx_c = tx.clone();
        let next_style = if s.style == "3d" { "2d" } else { "3d" };
        add_check(&look, "立体效果（3D）", s.style == "3d", move || {
            let _ = tx_c.send(MenuAction::SetValue {
                key: "style".into(),
                value: json!(next_style),
            });
        });
    }
    look.append(&gtk::SeparatorMenuItem::new());
    add_choice_str(&look, "眼睛颜色", "eyeColor", &s.eye, EYES, &tx);
    add_choice_str(&look, "眼睛高光", "eyeShine", &s.shine, SHINES, &tx);
    add_choice_str(&look, "手套颜色", "gloves", &s.gloves, GLOVES, &tx);
    add_choice_str(&look, "项圈", "collar", &s.collar, COLLARS, &tx);
    add_toggle(&look, "腮红", "blush", s.blush, &tx);
    add_toggle(&look, "胡须", "whiskers", s.whiskers, &tx);
    add_toggle(&look, "节日装扮", "festival", s.festival, &tx);
    add_submenu(&menu, "外观", &look);

    // 桌面
    let desk = gtk::Menu::new();
    add_choice_str(&desk, "键盘颜色", "keyboardColor", &s.keyboard_color, KEYBOARD_COLORS, &tx);
    add_toggle(&desk, "鼠标线", "mouseCable", s.mouse_cable, &tx);
    add_toggle(&desk, "桌上的杯子（它会推下去）", "cup", s.cup, &tx);
    add_choice_f64(&desk, "透明度", "opacity", s.opacity, OPACITIES, &tx);
    add_submenu(&menu, "桌面", &desk);

    // 习性
    let habits = gtk::Menu::new();
    add_choice_f64(&habits, "多久睡着", "sleepAfter", s.sleep_after, SLEEP_OPTIONS, &tx);
    add_toggle(&habits, "声音（摸它时呼噜）", "sound", s.sound, &tx);
    add_action(&habits, "调节反应…", &tx, MenuAction::OpenSettings);
    add_submenu(&menu, "习性", &habits);

    // 第二只猫
    let second = gtk::Menu::new();
    {
        let tx_c = tx.clone();
        add_check(&second, "陪着它", s.second_cat, move || {
            let _ = tx_c.send(MenuAction::ToggleSecondCat);
        });
    }
    add_choice_str(&second, "它的毛色", "cat2.coat", &s.coat2, COATS, &tx);
    add_choice_str(&second, "怎么相处", "pairMode", &s.pair_mode, PAIR_MODES, &tx);
    add_submenu(&menu, "第二只猫", &second);

    menu.append(&gtk::SeparatorMenuItem::new());
    add_toggle(&menu, "隐私模式（不显示键名）", "privacy", s.privacy, &tx);
    add_toggle(
        &menu,
        "鼠标穿透（点击直接落到下层窗口）",
        "clickThrough",
        s.click_through,
        &tx,
    );
    add_action(&menu, "回到右下角", &tx, MenuAction::ResetHome);
    add_action(
        &menu,
        if snap.visible { "隐藏" } else { "显示" },
        &tx,
        MenuAction::ToggleVisibility,
    );

    menu.append(&gtk::SeparatorMenuItem::new());
    {
        let tx_c = tx.clone();
        add_check(&menu, "开机自启", snap.autostart, move || {
            let _ = tx_c.send(MenuAction::ToggleAutostart);
        });
    }

    if !snap.hook_ready {
        menu.append(&gtk::SeparatorMenuItem::new());
        let item = gtk::MenuItem::with_label(
            snap.hook_error
                .as_deref()
                .unwrap_or("键盘监听未启动（请加入 input 组）"),
        );
        item.set_sensitive(false);
        menu.append(&item);
    }

    menu.append(&gtk::SeparatorMenuItem::new());
    add_action(&menu, "退出 KeyCat", &tx, MenuAction::Quit);

    menu.show_all();
    menu
}
