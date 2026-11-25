//! KeyCat native Linux host for Fedora KDE Wayland (aligned with KeyCat 1.1.0).
//! Uses GTK3 + gtk-layer-shell (Wayland Top layer) + WebKitGTK 4.1 + ksni (KDE SNI tray)
//! + evdev (global keyboard/mouse listener), reusing ../web directly.

mod autostart;
mod hook;
mod keymap;
mod menu;
mod state;
mod store;
mod tray;

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cairo::{Operator, RectangleInt, Region};
use gdk::RGBA;
use gio::prelude::*;
use gtk::prelude::*;
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use javascriptcore::ValueExt;
use serde_json::{json, Value};
use webkit2gtk::{
    LoadEvent, SettingsExt, URISchemeRequestExt, UserContentInjectedFrames, UserContentManager,
    UserContentManagerExt, UserScript, UserScriptInjectionTime, WebContext, WebContextExt,
    WebView, WebViewExt,
};

use hook::{Button, Input};
use state::{MenuAction, MenuSnapshot, Settings, DEFAULT_MARGIN_BOTTOM, DEFAULT_MARGIN_RIGHT};
use store::Store;
use tray::TrayController;

const SCHEME: &str = "keycat";

// Injected into index.html to forward in-window pointer events to the GTK/Wayland host
// and disable WebKit's default context menu, while keeping web/ completely untouched.
const PANEL_INTERACTION_JS: &str = r#"(() => {
  window.addEventListener('contextmenu', e => {
    e.preventDefault();
  });
  let lastSync = 0;
  window.addEventListener('pointermove', e => {
    if (window.cat && typeof window.cat.cursor === 'function') {
      window.cat.cursor(e.clientX, e.clientY);
    }
    const now = performance.now();
    if ((e.buttons & 1) || now - lastSync > 60) {
      lastSync = now;
      window.webkit?.messageHandlers?.cat?.postMessage({
        pointerMove: true,
        x: e.clientX,
        y: e.clientY,
      });
    }
  });
  window.addEventListener('pointerdown', e => {
    if (e.button === 0 && window.cat && typeof window.cat.hitTest === 'function') {
      const hit = window.cat.hitTest(e.clientX, e.clientY);
      window.webkit?.messageHandlers?.cat?.postMessage({
        pointerDown: true,
        hit,
        x: e.clientX,
        y: e.clientY,
      });
    }
  });
  window.addEventListener('pointerup', e => {
    if (e.button === 0) {
      window.webkit?.messageHandlers?.cat?.postMessage({ pointerUp: true });
    }
  });
})();"#;

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("keycat")
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("keycat")
}

fn picture_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Pictures)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")))
        .unwrap_or_else(std::env::temp_dir)
        .join("KeyCat")
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

fn log(text: &str) {
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("log.txt");
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > 1 << 20) {
        let _ = fs::rename(&path, dir.join("log.old.txt"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} {text}", timestamp());
    }
}

fn locate_web_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KEYCAT_WEB_DIR") {
        let p = PathBuf::from(dir);
        if p.join("index.html").is_file() {
            return p;
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("web"));
            candidates.push(dir.join("../web"));
            candidates.push(dir.join("../../web"));
            candidates.push(dir.join("../../../web"));
            candidates.push(dir.join("../share/keycat/web"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("web"));
        candidates.push(cwd.join("../web"));
    }
    candidates.push(PathBuf::from("/usr/share/keycat/web"));

    for path in candidates {
        if path.join("index.html").is_file() {
            return path.canonicalize().unwrap_or(path);
        }
    }
    PathBuf::from("../web")
}

fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn register_web_scheme(context: &WebContext, web_dir: PathBuf) {
    context.register_uri_scheme(SCHEME, move |req| {
        let uri = req.uri().map(|s| s.to_string()).unwrap_or_default();
        let rel = uri
            .strip_prefix("keycat://app/")
            .unwrap_or("index.html")
            .split('?')
            .next()
            .unwrap_or("index.html")
            .trim_start_matches('/');
        if rel.contains("..") {
            req.finish_error(&mut glib::Error::new(
                gio::IOErrorEnum::PermissionDenied,
                "invalid path",
            ));
            return;
        }
        let target = web_dir.join(if rel.is_empty() { "index.html" } else { rel });
        match fs::read(&target) {
            Ok(bytes) => {
                let len = bytes.len() as i64;
                let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                req.finish(&stream, len, Some(mime_type(&target)));
            }
            Err(err) => {
                req.finish_error(&mut glib::Error::new(
                    gio::IOErrorEnum::NotFound,
                    &format!("failed to read {}: {err}", target.display()),
                ));
            }
        }
    });
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let clean: Vec<u8> = text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let v: Vec<u8> = chunk.iter().map(|&c| value(c)).collect::<Option<_>>()?;
        let n = v
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &d)| acc | (u32::from(d) << (18 - 6 * i)));
        out.extend_from_slice(&n.to_be_bytes()[1..v.len()]);
    }
    Some(out)
}

fn save_picture(name: &str, base64: &str) {
    let safe = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-");
    let ext = safe.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if safe.starts_with('.') || !["gif", "png"].contains(&ext.as_str()) {
        return;
    }
    let Some(bytes) = decode_base64(base64) else {
        return;
    };
    let dir = picture_dir();
    let path = dir.join(&safe);
    match fs::create_dir_all(&dir).and_then(|_| fs::write(&path, bytes)) {
        Ok(()) => {
            log(&format!("saved {}", path.display()));
            let _ = Command::new("xdg-open").arg(&dir).spawn();
        }
        Err(err) => log(&format!("saving {safe} failed: {err}")),
    }
}

fn primary_monitor_index() -> i32 {
    gdk::Display::default()
        .map(|display| {
            let n = display.n_monitors();
            if let Some(primary) = display.primary_monitor() {
                for i in 0..n {
                    if display.monitor(i).as_ref() == Some(&primary) {
                        return i;
                    }
                }
            }
            0
        })
        .unwrap_or(0)
}

fn acquire_instance_lock() -> Option<File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("keycat.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Some(file)
    } else {
        None
    }
}

struct CatPanel {
    is_main: bool,
    window: gtk::ApplicationWindow,
    view: WebView,
    page_ready: bool,
    hit: String,
    petting: bool,
    dragging: bool,
    drag_grab: Option<(f64, f64)>,
    drag_pointer: Option<(f64, f64)>,
    pending_drag: (f64, f64),
    monitor_index: i32,
    margin_right: f64,
    margin_bottom: f64,
    virtual_cursor: (f64, f64),
    last_friend: Option<(i32, i32)>,
}

impl CatPanel {
    fn key_prefix(&self) -> &'static str {
        if self.is_main { "" } else { "cat2." }
    }

    fn eval(&self, script: &str, visible: bool) {
        if !self.page_ready || !visible {
            return;
        }
        self.view
            .evaluate_javascript(script, None, None, gio::Cancellable::NONE, |_| {});
    }

    fn apply_margins(&self, layer_shell_active: bool) {
        if layer_shell_active {
            self.window
                .set_layer_shell_margin(Edge::Right, self.margin_right.round() as i32);
            self.window
                .set_layer_shell_margin(Edge::Bottom, self.margin_bottom.round() as i32);
        }
    }

    fn apply_monitor(&self, layer_shell_active: bool) {
        if !layer_shell_active {
            return;
        }
        if let Some(display) = gdk::Display::default() {
            if let Some(mon) = display
                .monitor(self.monitor_index)
                .or_else(|| display.primary_monitor())
                .or_else(|| display.monitor(0))
            {
                self.window.set_monitor(&mon);
            }
        }
    }

    fn apply_input_region(&self, settings: &Settings) {
        let Some(gdk_win) = self.window.window() else {
            return;
        };
        if settings.click_through {
            gdk_win.input_shape_combine_region(&Region::create(), 0, 0);
            return;
        }
        let (w, h) = settings.window_size();
        let rect = RectangleInt::new(
            (w as f64 * 0.08).round() as i32,
            (h as f64 * 0.14).round() as i32,
            (w as f64 * 0.84).round() as i32,
            (h as f64 * 0.84).round() as i32,
        );
        let region = Region::create_rectangle(&rect);
        gdk_win.input_shape_combine_region(&region, 0, 0);
    }

    fn apply_scale(&self, settings: &Settings) {
        let (w, h) = settings.window_size();
        self.window.set_size_request(w, h);
        self.window.resize(w, h);
        self.apply_input_region(settings);
    }

    fn global_head_pos(&self, settings: &Settings) -> Option<(f64, f64)> {
        let display = gdk::Display::default()?;
        let mon = display.monitor(self.monitor_index)?;
        let g = mon.geometry();
        let (w, h) = settings.window_size();
        let gx = f64::from(g.x() + g.width()) - self.margin_right - f64::from(w);
        let gy = f64::from(g.y() + g.height()) - self.margin_bottom - f64::from(h);
        Some((gx + f64::from(w) * 0.49, gy + f64::from(h) * 0.36))
    }

    fn global_to_local(&self, settings: &Settings, gx: f64, gy: f64) -> Option<(i32, i32)> {
        let display = gdk::Display::default()?;
        let mon = display.monitor(self.monitor_index)?;
        let g = mon.geometry();
        let (w, h) = settings.window_size();
        let origin_x = f64::from(g.x() + g.width()) - self.margin_right - f64::from(w);
        let origin_y = f64::from(g.y() + g.height()) - self.margin_bottom - f64::from(h);
        Some(((gx - origin_x).round() as i32, (gy - origin_y).round() as i32))
    }

    fn drag_by(&mut self, dx: f64, dy: f64, layer_shell_active: bool, settings: &Settings) {
        if !layer_shell_active {
            return;
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let n = display.n_monitors();
        if n <= 0 {
            return;
        }
        if self.monitor_index < 0 || self.monitor_index >= n {
            self.monitor_index = primary_monitor_index();
        }
        let Some(cur_mon) = display.monitor(self.monitor_index) else {
            return;
        };
        let cur = cur_mon.geometry();
        let (win_w, win_h) = settings.window_size();
        let win_w = f64::from(win_w);
        let win_h = f64::from(win_h);

        let next_mr = self.margin_right - dx;
        let next_mb = self.margin_bottom - dy;

        let gx = f64::from(cur.x() + cur.width()) - next_mr - win_w;
        let gy = f64::from(cur.y() + cur.height()) - next_mb - win_h;
        let cx = gx + win_w * 0.5;
        let cy = gy + win_h * 0.5;

        let mut target_idx = self.monitor_index;
        let mut has_left = false;
        let mut has_right = false;
        let mut has_top = false;
        let mut has_bottom = false;
        for i in 0..n {
            if let Some(mon) = display.monitor(i) {
                let g = mon.geometry();
                let x0 = f64::from(g.x());
                let y0 = f64::from(g.y());
                let x1 = x0 + f64::from(g.width());
                let y1 = y0 + f64::from(g.height());
                if i != self.monitor_index {
                    if g.x() + g.width() <= cur.x() {
                        has_left = true;
                    }
                    if g.x() >= cur.x() + cur.width() {
                        has_right = true;
                    }
                    if g.y() + g.height() <= cur.y() {
                        has_top = true;
                    }
                    if g.y() >= cur.y() + cur.height() {
                        has_bottom = true;
                    }
                }
                if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
                    target_idx = i;
                }
            }
        }

        if target_idx != self.monitor_index {
            if let Some(next_mon) = display.monitor(target_idx) {
                let ng = next_mon.geometry();
                self.monitor_index = target_idx;
                let new_mr = f64::from(ng.x() + ng.width()) - (gx + win_w);
                let new_mb = f64::from(ng.y() + ng.height()) - (gy + win_h);
                self.margin_right = new_mr.clamp(-win_w * 0.5, f64::from(ng.width()) - win_w * 0.5);
                self.margin_bottom =
                    new_mb.clamp(-win_h * 0.5, f64::from(ng.height()) - win_h * 0.5);
                self.virtual_cursor = (win_w * 0.5, win_h * 0.5);
                self.window.set_monitor(&next_mon);
                self.apply_margins(layer_shell_active);
                self.apply_input_region(settings);
                return;
            }
        }

        let max_mr = if has_left {
            f64::from(cur.width()) - win_w * 0.4
        } else {
            (f64::from(cur.width()) - win_w).max(0.0)
        };
        let min_mr = if has_right { -win_w * 0.6 } else { 0.0 };
        let max_mb = if has_top {
            f64::from(cur.height()) - win_h * 0.4
        } else {
            (f64::from(cur.height()) - win_h).max(0.0)
        };
        let min_mb = if has_bottom { -win_h * 0.6 } else { 0.0 };
        self.margin_right = next_mr.clamp(min_mr, max_mr);
        self.margin_bottom = next_mb.clamp(min_mb, max_mb);
        self.apply_margins(layer_shell_active);
    }
}

struct HostState {
    app: gtk::Application,
    web_context: WebContext,
    store: Store,
    hook_ready: bool,
    hook_error: Option<String>,
    layer_shell_active: bool,
    visible: bool,
    pomodoro: bool,
    autostart: bool,
    pending_delta: (f64, f64),
    pending_dist: f64,
    held: HashSet<u16>,
    key_events: u64,
    panels: Vec<CatPanel>,
    settings_window: Option<gtk::Window>,
    stats_view: Option<(gtk::Window, WebView)>,
    context_menu: Option<gtk::Menu>,
    tray: Option<TrayController>,
    action_tx: glib::Sender<MenuAction>,
}

impl HostState {
    fn settings(&self) -> Settings {
        Settings::from_store(&self.store)
    }

    fn memory(&self) -> Value {
        self.store
            .get("memory")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn snapshot(&self) -> MenuSnapshot {
        MenuSnapshot {
            settings: self.settings(),
            visible: self.visible,
            pomodoro: self.pomodoro,
            autostart: self.autostart,
            hook_ready: self.hook_ready,
            hook_error: self.hook_error.clone(),
            layer_shell_active: self.layer_shell_active,
        }
    }

    fn sync_tray(&self) {
        if let Some(tray) = &self.tray {
            tray.update(self.snapshot());
        }
    }

    fn eval_main(&self, script: &str) {
        if let Some(panel) = self.panels.first() {
            panel.eval(script, self.visible);
        }
    }

    fn eval_all(&self, script: &str) {
        for panel in &self.panels {
            panel.eval(script, self.visible);
        }
    }

    fn push_options(&self) {
        let s = self.settings();
        for panel in &self.panels {
            let options = s.to_cat_options(panel.is_main, self.pomodoro);
            panel.eval(&format!("cat.options({options})"), true);
        }
    }

    fn apply_input_regions(&self) {
        let s = self.settings();
        for panel in &self.panels {
            panel.apply_input_region(&s);
        }
    }

    fn apply_scale(&mut self, value: f64) {
        let scale = value.clamp(0.4, 2.0);
        self.store.set("catScale", json!(scale));
        let s = self.settings();
        for panel in &self.panels {
            panel.apply_scale(&s);
        }
    }

    fn save_panel_margins(&mut self, idx: usize) {
        if let Some(panel) = self.panels.get(idx) {
            let p = panel.key_prefix();
            let mr = panel.margin_right.round() as i32;
            let mb = panel.margin_bottom.round() as i32;
            let mon = panel.monitor_index;
            self.store.set(&format!("{p}marginRight"), json!(mr));
            self.store.set(&format!("{p}marginBottom"), json!(mb));
            self.store.set(&format!("{p}monitorIndex"), json!(mon));
        }
    }

    fn refresh_stats_window(&self) {
        self.eval_main("cat.saveMemory(); cat.portrait()");
        self.notify_stats_memory();
    }

    fn notify_stats_memory(&self) {
        if let Some((_, view)) = &self.stats_view {
            let mem = self.memory();
            view.evaluate_javascript(
                &format!("initStats({mem})"),
                None,
                None,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }
}

fn configure_transparent_window(window: &impl IsA<gtk::Widget>) {
    if let Some(screen) = gtk::prelude::WidgetExt::screen(window) {
        if let Some(visual) = screen.rgba_visual() {
            window.set_visual(Some(&visual));
        }
    }
    window.set_app_paintable(true);
    window.connect_draw(|_, cr| {
        cr.set_operator(Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        glib::Propagation::Proceed
    });
}

fn create_cat_panel(state_rc: &Rc<RefCell<HostState>>, is_main: bool) {
    let (app, web_context, settings, layer_shell_active, visible) = {
        let st = state_rc.borrow();
        (
            st.app.clone(),
            st.web_context.clone(),
            st.settings(),
            st.layer_shell_active,
            st.visible,
        )
    };
    let (win_w, win_h) = settings.window_size();
    let monitor_index = {
        let wanted = if is_main {
            settings.monitor_index
        } else {
            settings.monitor_index_2.or(settings.monitor_index)
        };
        gdk::Display::default()
            .and_then(|display| {
                let n = display.n_monitors();
                wanted.filter(|&i| i >= 0 && i < n)
            })
            .unwrap_or_else(primary_monitor_index)
    };
    let (margin_right, margin_bottom) = if is_main {
        (settings.margin_right, settings.margin_bottom)
    } else {
        (
            settings
                .margin_right_2
                .unwrap_or(settings.margin_right + win_w + 16),
            settings.margin_bottom_2.unwrap_or(settings.margin_bottom),
        )
    };

    let panel_window = gtk::ApplicationWindow::new(&app);
    panel_window.set_title(if is_main { "KeyCat" } else { "KeyCat 2" });
    panel_window.set_decorated(false);
    panel_window.set_resizable(false);
    panel_window.set_skip_taskbar_hint(true);
    panel_window.set_skip_pager_hint(true);
    panel_window.set_size_request(win_w, win_h);
    panel_window.set_default_size(win_w, win_h);

    if layer_shell_active {
        panel_window.init_layer_shell();
        panel_window.set_namespace(if is_main { "keycat" } else { "keycat-2" });
        panel_window.set_layer(Layer::Top);
        panel_window.set_keyboard_mode(KeyboardMode::None);
        if let Some(mon) = gdk::Display::default().and_then(|d| d.monitor(monitor_index)) {
            panel_window.set_monitor(&mon);
        }
        panel_window.set_anchor(Edge::Right, true);
        panel_window.set_anchor(Edge::Bottom, true);
        panel_window.set_layer_shell_margin(Edge::Right, margin_right);
        panel_window.set_layer_shell_margin(Edge::Bottom, margin_bottom);
    } else {
        panel_window.set_keep_above(true);
        panel_window.set_accept_focus(false);
    }

    configure_transparent_window(&panel_window);

    let user_content = UserContentManager::new();
    user_content.add_script(&UserScript::new(
        PANEL_INTERACTION_JS,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    user_content.register_script_message_handler("cat");

    let panel_view = WebView::builder()
        .web_context(&web_context)
        .user_content_manager(&user_content)
        .build();
    panel_view.set_background_color(&RGBA::new(0.0, 0.0, 0.0, 0.0));
    if let Some(ws) = WebViewExt::settings(&panel_view) {
        ws.set_enable_javascript(true);
        ws.set_enable_webgl(true);
        ws.set_enable_smooth_scrolling(false);
        ws.set_media_playback_requires_user_gesture(false);
    }
    panel_window.add(&panel_view);

    let panel = CatPanel {
        is_main,
        window: panel_window.clone(),
        view: panel_view.clone(),
        page_ready: false,
        hit: "none".into(),
        petting: false,
        dragging: false,
        drag_grab: None,
        drag_pointer: None,
        pending_drag: (0.0, 0.0),
        monitor_index,
        margin_right: f64::from(margin_right),
        margin_bottom: f64::from(margin_bottom),
        virtual_cursor: (f64::from(win_w) * 0.5, f64::from(win_h) * 0.5),
        last_friend: None,
    };

    {
        let mut st = state_rc.borrow_mut();
        if is_main {
            st.panels.insert(0, panel);
        } else {
            st.panels.push(panel);
        }
    }

    // Script message handler for this cat panel
    {
        let state_clone = state_rc.clone();
        user_content.connect_script_message_received(Some("cat"), move |_, result| {
            let Some(json_str) = result.js_value().and_then(|v| v.to_json(0)) else {
                return;
            };
            let Ok(msg) = serde_json::from_str::<Value>(json_str.as_str()) else {
                return;
            };
            let mut st = state_clone.borrow_mut();
            let Some(idx) = st.panels.iter().position(|p| p.is_main == is_main) else {
                return;
            };

            if let Some(hit) = msg.get("hit").and_then(Value::as_str) {
                st.panels[idx].hit = hit.to_string();
            }
            if msg.get("pointerMove").and_then(Value::as_bool) == Some(true) {
                let x = msg.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = msg.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                st.panels[idx].virtual_cursor = (x, y);
                if st.panels[idx].petting {
                    st.panels[idx].eval(&format!("cat.pet(true, {})", x.round()), true);
                }
                return;
            }
            if msg.get("pointerDown").and_then(Value::as_bool) == Some(true) {
                if st.settings().click_through {
                    return;
                }
                let hit = msg
                    .get("hit")
                    .and_then(Value::as_str)
                    .unwrap_or(&st.panels[idx].hit)
                    .to_string();
                let x = msg.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = msg.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                if hit == "pet" {
                    st.panels[idx].petting = true;
                    st.panels[idx].eval(&format!("cat.pet(true, {})", x.round()), true);
                } else if hit == "drag" {
                    st.panels[idx].dragging = true;
                    st.panels[idx].drag_grab = Some((x, y));
                    st.panels[idx].drag_pointer = None;
                    st.panels[idx].pending_drag = (0.0, 0.0);
                }
                return;
            }
            if msg.get("pointerUp").and_then(Value::as_bool) == Some(true) {
                if st.panels[idx].petting {
                    st.panels[idx].petting = false;
                    st.panels[idx].eval("cat.pet(false, 0)", true);
                }
                if st.panels[idx].dragging {
                    st.panels[idx].dragging = false;
                    st.panels[idx].drag_grab = None;
                    st.panels[idx].drag_pointer = None;
                    st.panels[idx].pending_drag = (0.0, 0.0);
                    st.save_panel_margins(idx);
                }
                return;
            }
            if let Some(ticks) = msg.get("ticks").and_then(Value::as_f64) {
                if is_main {
                    let line = format!(
                        "{} keyboard={} keyEvents={} fps={}\n",
                        timestamp(),
                        if st.hook_ready { "active" } else { "missing" },
                        st.key_events,
                        ticks.round()
                    );
                    let _ = fs::create_dir_all(data_dir());
                    let _ = fs::write(data_dir().join("status.txt"), line);
                }
            } else if let Some(memory) = msg.get("memory").filter(|m| m.is_object() && is_main) {
                st.store.set("memory", memory.clone());
                st.notify_stats_memory();
            } else if let Some(file) = msg.get("saveFile") {
                if let (Some(name), Some(data)) = (
                    file.get("name").and_then(Value::as_str),
                    file.get("base64").and_then(Value::as_str),
                ) {
                    save_picture(name, data);
                }
            } else if let Some(portrait) = msg.get("portrait").and_then(Value::as_str) {
                if portrait.starts_with("data:image/png;base64,") {
                    if let Some((_, view)) = &st.stats_view {
                        let script = format!("showPortrait({})", json!(portrait));
                        view.evaluate_javascript(
                            &script,
                            None,
                            None,
                            gio::Cancellable::NONE,
                            |_| {},
                        );
                    }
                }
            } else if let Some(event) = msg
                .get("friend")
                .and_then(Value::as_str)
                .filter(|e| ["swat", "blink"].contains(e))
            {
                let script = format!("cat.friendEvent('{event}')");
                for other in &st.panels {
                    if other.is_main != is_main {
                        other.eval(&script, st.visible);
                    }
                }
            }
        });
    }

    // Track in-window pointer coordinates during drag
    {
        let state_clone = state_rc.clone();
        panel_view.connect_motion_notify_event(move |_, event| {
            let mut st = state_clone.borrow_mut();
            if let Some(p) = st.panels.iter_mut().find(|p| p.is_main == is_main) {
                if p.dragging {
                    p.drag_pointer = Some(event.position());
                }
            }
            glib::Propagation::Proceed
        });
    }

    // Native GTK right-click handler on WebView
    {
        let state_clone = state_rc.clone();
        panel_view.connect_button_press_event(move |_, event| {
            if event.button() == 3 {
                let mut st = state_clone.borrow_mut();
                if st.settings().click_through {
                    return glib::Propagation::Proceed;
                }
                let snap = st.snapshot();
                let menu = menu::build_context_menu(&snap, st.action_tx.clone());
                menu.popup_at_pointer(Some(event));
                st.context_menu = Some(menu);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }

    // Initialize memory and options on page load finished
    {
        let state_clone = state_rc.clone();
        panel_view.connect_load_changed(move |_, event| {
            if event == LoadEvent::Finished {
                let mut st = state_clone.borrow_mut();
                let mem = st.memory();
                let opts = st.settings().to_cat_options(is_main, st.pomodoro);
                if let Some(p) = st.panels.iter_mut().find(|p| p.is_main == is_main) {
                    p.page_ready = true;
                    p.last_friend = None;
                    if is_main {
                        p.eval(&format!("cat.memory({mem})"), true);
                    }
                    p.eval(&format!("cat.options({opts})"), true);
                }
                log(&format!("index.html loaded (main={is_main})"));
            }
        });
    }

    {
        let state_clone = state_rc.clone();
        panel_window.connect_map_event(move |_, _| {
            let st = state_clone.borrow();
            let s = st.settings();
            if let Some(p) = st.panels.iter().find(|p| p.is_main == is_main) {
                p.apply_input_region(&s);
            }
            glib::Propagation::Proceed
        });
    }

    panel_view.load_uri("keycat://app/index.html");
    if visible {
        panel_window.show_all();
        state_rc
            .borrow()
            .panels
            .iter()
            .find(|p| p.is_main == is_main)
            .inspect(|p| p.apply_input_region(&settings));
    }
}

fn open_settings_window(state_rc: &Rc<RefCell<HostState>>) {
    let context = {
        let st = state_rc.borrow();
        if let Some(win) = &st.settings_window {
            win.present();
            return;
        }
        st.web_context.clone()
    };

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("KeyCat 调节");
    win.set_default_size(420, 820);
    win.set_resizable(false);

    let manager = UserContentManager::new();
    let state_for_msg = state_rc.clone();
    manager.connect_script_message_received(Some("settings"), move |_, result| {
        if let Some(text) = result.js_value().and_then(|v| v.to_json(0)) {
            if let Ok(body) = serde_json::from_str::<Value>(text.as_str()) {
                let mut st = state_for_msg.borrow_mut();
                if let Some(tune) = body.get("tune").and_then(Value::as_object) {
                    let mut merged = st.settings().tune;
                    if let Some(saved) = merged.as_object_mut() {
                        saved.extend(tune.clone());
                    }
                    st.store.set("tune", merged);
                    st.push_options();
                } else if let Some(preset) = body.get("preset").filter(|p| p.is_object()) {
                    st.store.set("tune", preset.clone());
                    st.push_options();
                } else if body.get("reset").and_then(Value::as_bool) == Some(true) {
                    st.store.set("tune", json!({}));
                    st.push_options();
                } else if let Some(test) = body
                    .get("test")
                    .and_then(Value::as_str)
                    .filter(|t| t.bytes().all(|b| b.is_ascii_alphabetic()))
                {
                    st.eval_main(&format!("cat.test('{test}')"));
                }
            }
        }
    });
    manager.register_script_message_handler("settings");

    let webview = WebView::builder()
        .web_context(&context)
        .user_content_manager(&manager)
        .build();
    if let Some(ws) = WebViewExt::settings(&webview) {
        ws.set_enable_javascript(true);
    }

    let state_for_load = state_rc.clone();
    webview.connect_load_changed(move |view, event| {
        if event == LoadEvent::Finished {
            let tune = state_for_load.borrow().settings().tune;
            view.evaluate_javascript(
                &format!("initTune({tune})"),
                None,
                None,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    });

    webview.load_uri("keycat://app/settings.html");
    win.add(&webview);

    let state_for_close = state_rc.clone();
    win.connect_delete_event(move |w, _| {
        w.hide();
        state_for_close.borrow_mut().settings_window = None;
        glib::Propagation::Proceed
    });

    win.show_all();
    state_rc.borrow_mut().settings_window = Some(win);
}

fn open_stats_window(state_rc: &Rc<RefCell<HostState>>) {
    let context = {
        let st = state_rc.borrow();
        if let Some((win, _)) = &st.stats_view {
            win.present();
            st.refresh_stats_window();
            return;
        }
        st.web_context.clone()
    };

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("KeyCat 今日小结");
    win.set_default_size(380, 720);
    win.set_resizable(false);

    let manager = UserContentManager::new();
    let state_for_msg = state_rc.clone();
    manager.connect_script_message_received(Some("stats"), move |_, result| {
        if let Some(text) = result.js_value().and_then(|v| v.to_json(0)) {
            if let Ok(body) = serde_json::from_str::<Value>(text.as_str()) {
                if let Some(save) = body.get("save") {
                    if let (Some(name), Some(data)) = (
                        save.get("name").and_then(Value::as_str),
                        save.get("base64").and_then(Value::as_str),
                    ) {
                        save_picture(name, data);
                    }
                } else if body.get("refresh").is_some() {
                    state_for_msg.borrow().refresh_stats_window();
                }
            }
        }
    });
    manager.register_script_message_handler("stats");

    let webview = WebView::builder()
        .web_context(&context)
        .user_content_manager(&manager)
        .build();
    if let Some(ws) = WebViewExt::settings(&webview) {
        ws.set_enable_javascript(true);
    }

    let state_for_load = state_rc.clone();
    webview.connect_load_changed(move |_, event| {
        if event == LoadEvent::Finished {
            state_for_load.borrow().refresh_stats_window();
        }
    });

    webview.load_uri("keycat://app/stats.html");
    win.add(&webview);

    let state_for_close = state_rc.clone();
    win.connect_delete_event(move |w, _| {
        w.hide();
        state_for_close.borrow_mut().stats_view = None;
        glib::Propagation::Proceed
    });

    win.show_all();
    state_rc.borrow_mut().stats_view = Some((win, webview));
}

fn handle_menu_action(state_rc: &Rc<RefCell<HostState>>, action: MenuAction) {
    match action {
        MenuAction::SetValue { key, value } => {
            let mut st = state_rc.borrow_mut();
            st.store.set(&key, value);
            st.push_options();
            st.apply_input_regions();
            st.sync_tray();
        }
        MenuAction::SetScale(scale) => {
            let mut st = state_rc.borrow_mut();
            st.apply_scale(scale);
            st.sync_tray();
        }
        MenuAction::TogglePomodoro => {
            let mut st = state_rc.borrow_mut();
            st.pomodoro = !st.pomodoro;
            st.push_options();
            st.sync_tray();
        }
        MenuAction::OpenStats => {
            open_stats_window(state_rc);
        }
        MenuAction::RecordGif => {
            state_rc.borrow().eval_main("cat.record(3)");
        }
        MenuAction::ToggleSecondCat => {
            let enable = {
                let mut st = state_rc.borrow_mut();
                let next = !st.settings().second_cat;
                st.store.set("secondCat", json!(next));
                next
            };
            if enable {
                let already = state_rc.borrow().panels.iter().any(|p| !p.is_main);
                if !already {
                    create_cat_panel(state_rc, false);
                }
            } else {
                let removed = {
                    let mut st = state_rc.borrow_mut();
                    let pos = st.panels.iter().position(|p| !p.is_main);
                    pos.map(|i| st.panels.remove(i))
                };
                if let Some(panel) = removed {
                    panel.window.close();
                }
            }
            let st = state_rc.borrow();
            st.push_options();
            st.sync_tray();
        }
        MenuAction::OpenSettings => {
            open_settings_window(state_rc);
        }
        MenuAction::ToggleAutostart => {
            let mut st = state_rc.borrow_mut();
            let target = !st.autostart;
            match autostart::set_enabled(target) {
                Ok(()) => st.autostart = target,
                Err(err) => log(&format!("autostart toggle failed: {err}")),
            }
            st.sync_tray();
        }
        MenuAction::ResetHome => {
            let mut st = state_rc.borrow_mut();
            let primary = primary_monitor_index();
            let layer_shell = st.layer_shell_active;
            let s = st.settings();
            let (win_w, _) = s.window_size();
            for key in [
                "marginRight",
                "marginBottom",
                "monitorIndex",
                "cat2.marginRight",
                "cat2.marginBottom",
                "cat2.monitorIndex",
            ] {
                st.store.remove(key);
            }
            for panel in &mut st.panels {
                panel.monitor_index = primary;
                panel.margin_right = f64::from(if panel.is_main {
                    DEFAULT_MARGIN_RIGHT
                } else {
                    DEFAULT_MARGIN_RIGHT + win_w + 16
                });
                panel.margin_bottom = f64::from(DEFAULT_MARGIN_BOTTOM);
                panel.apply_monitor(layer_shell);
                panel.apply_margins(layer_shell);
                panel.apply_input_region(&s);
            }
        }
        MenuAction::ToggleVisibility => {
            let mut st = state_rc.borrow_mut();
            st.visible = !st.visible;
            let show = st.visible;
            let s = st.settings();
            for panel in &st.panels {
                if show {
                    panel.window.show_all();
                    panel.apply_input_region(&s);
                } else {
                    panel.window.hide();
                }
            }
            st.sync_tray();
        }
        MenuAction::Quit => {
            state_rc.borrow().app.quit();
        }
    }
}

fn handle_evdev_input(state_rc: &Rc<RefCell<HostState>>, input: Input) {
    let mut st = state_rc.borrow_mut();
    match input {
        Input::Key { id, code, down } => {
            if down && !st.held.insert(id) {
                return;
            }
            if !down {
                st.held.remove(&id);
            }
            st.key_events += 1;
            if let Some(code) = code {
                st.eval_all(&format!("cat.key('{code}',{down})"));
            }
        }
        Input::Button { button, down } => {
            if !down && button == Button::Left {
                let mut save_indices = Vec::new();
                for (i, panel) in st.panels.iter_mut().enumerate() {
                    if panel.petting {
                        panel.petting = false;
                        panel.eval("cat.pet(false, 0)", true);
                    }
                    if panel.dragging {
                        panel.dragging = false;
                        panel.drag_grab = None;
                        panel.drag_pointer = None;
                        panel.pending_drag = (0.0, 0.0);
                        save_indices.push(i);
                    }
                }
                for i in save_indices {
                    st.save_panel_margins(i);
                }
            }
            st.eval_all(&format!("cat.mouseButton({down})"));
        }
        Input::Move { dx, dy } => {
            let step = dx.hypot(dy);
            let gain = (1.0 + step * 0.04).min(2.4);
            let mut any_dragging = false;
            for panel in &mut st.panels {
                if panel.dragging {
                    panel.pending_drag.0 += dx * gain;
                    panel.pending_drag.1 += dy * gain;
                    any_dragging = true;
                }
            }
            if any_dragging {
                return;
            }
            st.pending_delta.0 += dx * gain;
            st.pending_delta.1 += dy * gain;
            st.pending_dist += step * gain;
            let (win_w, win_h) = st.settings().window_size();
            let cx = f64::from(win_w) * 0.5;
            let cy = f64::from(win_h) * 0.5;
            for panel in &mut st.panels {
                panel.virtual_cursor.0 =
                    (panel.virtual_cursor.0 + dx * 0.8).clamp(cx - 240.0, cx + 240.0);
                panel.virtual_cursor.1 =
                    (panel.virtual_cursor.1 + dy * 0.8).clamp(cy - 200.0, cy + 200.0);
            }
        }
        Input::Wheel(notches) => {
            st.pending_delta.1 += -notches * 8.0;
        }
    }
}

fn build_app(app: &gtk::Application) -> Rc<RefCell<HostState>> {
    let web_dir = locate_web_dir();
    log(&format!("starting KeyCat Linux 1.1.0, web_dir={}", web_dir.display()));

    let store = Store::open(config_dir().join("settings.json"));
    let second_cat = Settings::from_store(&store).second_cat;

    let web_context = WebContext::default().expect("failed to get default WebContext");
    register_web_scheme(&web_context, web_dir);

    let layer_shell_active = gtk_layer_shell::is_supported();
    if layer_shell_active {
        log("gtk-layer-shell supported");
    } else {
        log("gtk-layer-shell not supported in current session, falling back to normal window");
    }

    #[allow(deprecated)]
    let (action_tx, action_rx) = glib::MainContext::channel::<MenuAction>(glib::Priority::DEFAULT);

    let (evdev_tx, evdev_rx) = mpsc::channel::<Input>();
    let (hook_ready, hook_error) = match hook::start(evdev_tx) {
        Ok(count) => {
            log(&format!("evdev hook started ({count} devices)"));
            (true, None)
        }
        Err(err) => {
            log(&format!("evdev hook failed: {err}"));
            (false, Some(err))
        }
    };

    let state_rc = Rc::new(RefCell::new(HostState {
        app: app.clone(),
        web_context,
        store,
        hook_ready,
        hook_error,
        layer_shell_active,
        visible: true,
        pomodoro: false,
        autostart: autostart::is_enabled(),
        pending_delta: (0.0, 0.0),
        pending_dist: 0.0,
        held: HashSet::new(),
        key_events: 0,
        panels: Vec::new(),
        settings_window: None,
        stats_view: None,
        context_menu: None,
        tray: None,
        action_tx: action_tx.clone(),
    }));

    create_cat_panel(&state_rc, true);
    if second_cat {
        create_cat_panel(&state_rc, false);
    }

    let initial_snap = state_rc.borrow().snapshot();
    let tray_ctrl = TrayController::start(initial_snap, action_tx);
    state_rc.borrow_mut().tray = Some(tray_ctrl);

    {
        let state_clone = state_rc.clone();
        action_rx.attach(None, move |action| {
            handle_menu_action(&state_clone, action);
            glib::ControlFlow::Continue
        });
    }

    // 60fps pump: drain evdev channel, flush drag, mouse movement, and friend head positions
    {
        let state_clone = state_rc.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            while let Ok(input) = evdev_rx.try_recv() {
                handle_evdev_input(&state_clone, input);
            }
            let mut st = state_clone.borrow_mut();
            let layer_shell = st.layer_shell_active;
            let visible = st.visible;
            let s = st.settings();

            for panel in &mut st.panels {
                if panel.dragging {
                    let (mut dx, mut dy) = panel.pending_drag;
                    panel.pending_drag = (0.0, 0.0);
                    if let (Some((gx, gy)), Some((px, py))) =
                        (panel.drag_grab, panel.drag_pointer.take())
                    {
                        let (ex, ey) = (px - gx, py - gy);
                        if ex.abs() > 2.0 {
                            dx += ex * 0.25;
                        }
                        if ey.abs() > 2.0 {
                            dy += ey * 0.25;
                        }
                    }
                    if dx.abs() >= 0.5 || dy.abs() >= 0.5 {
                        panel.drag_by(dx, dy, layer_shell, &s);
                    }
                }
            }

            let has_move = st.pending_delta != (0.0, 0.0) || st.pending_dist > 0.0;
            let (mut dx, mut dy) = st.pending_delta;
            let dist = st.pending_dist;
            st.pending_delta = (0.0, 0.0);
            st.pending_dist = 0.0;
            if has_move {
                let vec_dist = dx.hypot(dy);
                if dist > vec_dist + 1.0 {
                    if vec_dist > 0.1 {
                        let scale = dist / vec_dist;
                        dx *= scale;
                        dy *= scale;
                    } else {
                        dx = dist;
                    }
                }
            }

            let heads: Vec<Option<(f64, f64)>> = if st.panels.len() > 1 {
                st.panels.iter().map(|p| p.global_head_pos(&s)).collect()
            } else {
                Vec::new()
            };

            let len = st.panels.len();
            for (i, panel) in st.panels.iter_mut().enumerate() {
                let friend_local = if len > 1 {
                    let other_idx = 1 - i.min(1);
                    heads[other_idx].and_then(|(hx, hy)| panel.global_to_local(&s, hx, hy))
                } else {
                    None
                };

                if !panel.page_ready || !visible {
                    continue;
                }
                let mut calls = Vec::new();
                if has_move {
                    let (cx, cy) = panel.virtual_cursor;
                    calls.push(format!(
                        "cat.mouseMove({dx:.1},{dy:.1});cat.cursor({},{})",
                        cx.round(),
                        cy.round()
                    ));
                }
                if let Some(at) = friend_local {
                    if panel
                        .last_friend
                        .is_none_or(|f| (f.0 - at.0).abs() + (f.1 - at.1).abs() > 2)
                    {
                        panel.last_friend = Some(at);
                        calls.push(format!("cat.friend({},{})", at.0, at.1));
                    }
                }
                if !calls.is_empty() {
                    panel.eval(&calls.join(";"), visible);
                }
            }

            glib::ControlFlow::Continue
        });
    }

    // Periodic status.txt heartbeat every 15s
    {
        let state_clone = state_rc.clone();
        glib::timeout_add_local(Duration::from_secs(15), move || {
            state_clone.borrow().eval_main("cat.state.ticks = 0");
            let state_read = state_clone.clone();
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                state_read.borrow().eval_main(
                    "window.webkit?.messageHandlers?.cat?.postMessage({ ticks: cat.state.ticks })",
                );
            });
            glib::ControlFlow::Continue
        });
    }

    state_rc
}

fn main() {
    // Prevent WebKitGTK DMA-BUF transparent WebGL ghosting on Fedora KDE Wayland
    // unless the user explicitly overrides it.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    let app = gtk::Application::new(Some("com.ellobok.keycat"), gio::ApplicationFlags::empty());
    let instance_state: Rc<RefCell<Option<Rc<RefCell<HostState>>>>> = Rc::new(RefCell::new(None));
    let instance_lock: Rc<RefCell<Option<File>>> = Rc::new(RefCell::new(None));

    app.connect_activate(move |app| {
        if let Some(state_rc) = instance_state.borrow().as_ref() {
            let mut st = state_rc.borrow_mut();
            st.visible = true;
            let s = st.settings();
            for panel in &st.panels {
                panel.window.show_all();
                panel.apply_input_region(&s);
            }
            st.sync_tray();
            return;
        }
        let Some(lock) = acquire_instance_lock() else {
            eprintln!("KeyCat is already running.");
            app.quit();
            return;
        };
        *instance_lock.borrow_mut() = Some(lock);
        let state_rc = build_app(app);
        *instance_state.borrow_mut() = Some(state_rc);
    });
    app.run();
}
