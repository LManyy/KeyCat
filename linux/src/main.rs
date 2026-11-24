//! KeyCat native Linux host for Fedora KDE Wayland.
//! Uses GTK3 + gtk-layer-shell (Wayland overlay/top layer) + WebKitGTK 4.1 + ksni (KDE SNI tray)
//! + evdev (global keyboard/mouse listener), reusing ../web directly without modification.

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
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cairo::{Operator, RectangleInt, Region};
use gdk::RGBA;
use gio::prelude::*;
use gtk::prelude::*;
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde_json::{json, Value};
use javascriptcore::ValueExt;
use webkit2gtk::{
    LoadEvent, SettingsExt, URISchemeRequestExt, UserContentInjectedFrames, UserContentManager,
    UserContentManagerExt, UserScript, UserScriptInjectionTime, WebContext, WebContextExt,
    WebView, WebViewExt,
};

use hook::{Button, Input};
use state::{
    MenuAction, MenuSnapshot, Settings, DEFAULT_MARGIN_BOTTOM, DEFAULT_MARGIN_RIGHT, DESIGN,
};
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
    let file = OpenOptions::new().create(true).write(true).truncate(false).open(path).ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Some(file)
    } else {
        None
    }
}

struct HostState {
    store: Store,
    page_ready: bool,
    hook_ready: bool,
    hook_error: Option<String>,
    layer_shell_active: bool,
    visible: bool,
    autostart: bool,
    hit: String,
    petting: bool,
    dragging: bool,
    drag_grab: Option<(f64, f64)>,
    drag_pointer: Option<(f64, f64)>,
    pending_drag: (f64, f64),
    monitor_index: i32,
    margin_right: f64,
    margin_bottom: f64,
    pending_delta: (f64, f64),
    pending_dist: f64,
    virtual_cursor: (f64, f64),
    held: HashSet<u16>,
    key_events: u64,
    panel_window: gtk::ApplicationWindow,
    panel_view: WebView,
    settings_window: Option<gtk::Window>,
    context_menu: Option<gtk::Menu>,
    tray: Option<TrayController>,
    action_tx: glib::Sender<MenuAction>,
}

impl HostState {
    fn settings(&self) -> Settings {
        Settings::from_store(&self.store)
    }

    fn snapshot(&self) -> MenuSnapshot {
        MenuSnapshot {
            settings: self.settings(),
            visible: self.visible,
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

    fn eval_cat(&self, script: &str) {
        if !self.page_ready || !self.visible {
            return;
        }
        self.panel_view
            .evaluate_javascript(script, None, None, gio::Cancellable::NONE, |_| {});
    }

    fn push_options(&self) {
        let options = self.settings().to_cat_options();
        self.eval_cat(&format!("cat.options({options})"));
    }

    fn apply_margins(&self) {
        if self.layer_shell_active {
            self.panel_window
                .set_layer_shell_margin(Edge::Right, self.margin_right.round() as i32);
            self.panel_window
                .set_layer_shell_margin(Edge::Bottom, self.margin_bottom.round() as i32);
        }
    }

    fn apply_input_region(&self) {
        let Some(gdk_win) = self.panel_window.window() else {
            return;
        };
        let s = self.settings();
        if s.click_through {
            // Empty input region => 100% click-through to underlying windows.
            gdk_win.input_shape_combine_region(&Region::create(), 0, 0);
            return;
        }
        // Interactive region covering cat body + head + keyboard, while keeping
        // transparent top corners click-through.
        let (w, h) = s.window_size();
        let rect = RectangleInt::new(
            (w as f64 * 0.08).round() as i32,
            (h as f64 * 0.14).round() as i32,
            (w as f64 * 0.84).round() as i32,
            (h as f64 * 0.84).round() as i32,
        );
        let region = Region::create_rectangle(&rect);
        gdk_win.input_shape_combine_region(&region, 0, 0);
    }

    fn apply_scale(&mut self, value: f64) {
        let scale = value.clamp(0.4, 2.0);
        self.store.set("catScale", json!(scale));
        let (w, h) = (
            (DESIGN.0 * scale).round() as i32,
            (DESIGN.1 * scale).round() as i32,
        );
        self.panel_window.set_size_request(w, h);
        self.panel_window.resize(w, h);
        self.apply_input_region();
    }

    fn save_margins(&mut self) {
        self.store
            .set("marginRight", json!(self.margin_right.round() as i32));
        self.store
            .set("marginBottom", json!(self.margin_bottom.round() as i32));
        self.store
            .set("monitorIndex", json!(self.monitor_index));
    }

    fn current_monitor(&self) -> Option<gdk::Monitor> {
        let display = gdk::Display::default()?;
        display
            .monitor(self.monitor_index)
            .or_else(|| display.primary_monitor())
            .or_else(|| display.monitor(0))
    }

    fn apply_monitor(&self) {
        if self.layer_shell_active {
            if let Some(mon) = self.current_monitor() {
                self.panel_window.set_monitor(&mon);
            }
        }
    }

    /// Dragging across monitors: compute global center position from current monitor + margins;
    /// if the center moves into another monitor's geometry, switch the layer-shell output to that
    /// monitor and convert margins relative to the new monitor's bottom-right edge.
    fn drag_by(&mut self, dx: f64, dy: f64) {
        if !self.layer_shell_active {
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
        let (win_w, win_h) = self.settings().window_size();
        let win_w = f64::from(win_w);
        let win_h = f64::from(win_h);

        let next_mr = self.margin_right - dx;
        let next_mb = self.margin_bottom - dy;

        // Proposed window top-left and center in global logical desktop coordinates
        let gx = f64::from(cur.x() + cur.width()) - next_mr - win_w;
        let gy = f64::from(cur.y() + cur.height()) - next_mb - win_h;
        let cx = gx + win_w * 0.5;
        let cy = gy + win_h * 0.5;

        // Detect adjacent monitors around current monitor and check if center landed on another monitor
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
                self.margin_bottom = new_mb.clamp(-win_h * 0.5, f64::from(ng.height()) - win_h * 0.5);
                self.virtual_cursor = (win_w * 0.5, win_h * 0.5);
                self.panel_window.set_monitor(&next_mon);
                self.apply_margins();
                self.apply_input_region();
                return;
            }
        }

        // Only allow overhang past edges that have an adjacent monitor; keep outer edges inside screen
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
        self.apply_margins();
    }
}

fn configure_transparent_window(window: &impl IsA<gtk::Widget>) {
    if let Some(screen) = gtk::prelude::WidgetExt::screen(window) {
        if let Some(visual) = screen.rgba_visual() {
            window.set_visual(Some(&visual));
        }
    }
    window.set_app_paintable(true);
    // Clear the surface to full transparent on every draw pass to prevent WebKitGTK ghosting.
    window.connect_draw(|_, cr| {
        cr.set_operator(Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        glib::Propagation::Proceed
    });
}

fn open_settings_window(state_rc: &Rc<RefCell<HostState>>, context: &WebContext) {
    {
        let state = state_rc.borrow();
        if let Some(win) = &state.settings_window {
            win.present();
            return;
        }
    }

    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("KeyCat 调节");
    win.set_default_size(420, 760);
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
                } else if body.get("reset").and_then(Value::as_bool) == Some(true) {
                    st.store.set("tune", json!({}));
                    st.push_options();
                } else if let Some(test) = body.get("test").and_then(Value::as_str) {
                    st.eval_cat(if test == "annoy" {
                        "cat.annoyNow()"
                    } else {
                        "cat.curiousNow(3)"
                    });
                }
            }
        }
    });
    manager.register_script_message_handler("settings");

    let webview = WebView::builder()
        .web_context(context)
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

fn handle_menu_action(
    app: &gtk::Application,
    state_rc: &Rc<RefCell<HostState>>,
    context: &WebContext,
    action: MenuAction,
) {
    match action {
        MenuAction::SetValue { key, value } => {
            let mut st = state_rc.borrow_mut();
            st.store.set(&key, value);
            st.push_options();
            st.apply_input_region();
            st.sync_tray();
        }
        MenuAction::SetScale(scale) => {
            let mut st = state_rc.borrow_mut();
            st.apply_scale(scale);
            st.sync_tray();
        }
        MenuAction::OpenSettings => {
            open_settings_window(state_rc, context);
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
            st.monitor_index = primary_monitor_index();
            st.margin_right = f64::from(DEFAULT_MARGIN_RIGHT);
            st.margin_bottom = f64::from(DEFAULT_MARGIN_BOTTOM);
            st.store.remove("marginRight");
            st.store.remove("marginBottom");
            st.store.remove("monitorIndex");
            st.apply_monitor();
            st.apply_margins();
            st.apply_input_region();
        }
        MenuAction::ToggleVisibility => {
            let mut st = state_rc.borrow_mut();
            st.visible = !st.visible;
            if st.visible {
                st.panel_window.show_all();
                st.apply_input_region();
            } else {
                st.panel_window.hide();
            }
            st.sync_tray();
        }
        MenuAction::Quit => {
            app.quit();
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
                st.eval_cat(&format!("cat.key('{code}',{down})"));
            }
        }
        Input::Button { button, down } => {
            if !down && button == Button::Left {
                if st.petting {
                    st.petting = false;
                    st.eval_cat("cat.pet(false, 0)");
                }
                if st.dragging {
                    st.dragging = false;
                    st.drag_grab = None;
                    st.drag_pointer = None;
                    st.pending_drag = (0.0, 0.0);
                    st.save_margins();
                }
            }
            st.eval_cat(&format!("cat.mouseButton({down})"));
        }
        Input::Move { dx, dy } => {
            // Approximate pointer acceleration for raw evdev counts so rapid movements
            // on any monitor (including secondary displays) trigger curious reactions cleanly.
            let step = dx.hypot(dy);
            let gain = (1.0 + step * 0.04).min(2.4);
            if st.dragging {
                // Accumulate evdev feedforward delta; flushed at most once per frame to avoid
                // flooding Wayland with commits on 1000Hz mice.
                st.pending_drag.0 += dx * gain;
                st.pending_drag.1 += dy * gain;
                return;
            }
            st.pending_delta.0 += dx * gain;
            st.pending_delta.1 += dy * gain;
            st.pending_dist += step * gain;
            // Maintain a tight virtual cursor around the cat's center (full gaze saturation is ~220px)
            // so reversing mouse direction immediately turns the cat's gaze without crossing a huge deadzone.
            let (win_w, win_h) = st.settings().window_size();
            let cx = f64::from(win_w) * 0.5;
            let cy = f64::from(win_h) * 0.5;
            st.virtual_cursor.0 = (st.virtual_cursor.0 + dx * 0.8).clamp(cx - 240.0, cx + 240.0);
            st.virtual_cursor.1 = (st.virtual_cursor.1 + dy * 0.8).clamp(cy - 200.0, cy + 200.0);
        }
        Input::Wheel(notches) => {
            st.pending_delta.1 += -notches * 8.0;
        }
    }
}

fn build_app(app: &gtk::Application) -> Rc<RefCell<HostState>> {
    let web_dir = locate_web_dir();
    log(&format!("starting KeyCat Linux, web_dir={}", web_dir.display()));

    let store = Store::open(config_dir().join("settings.json"));
    let initial_settings = Settings::from_store(&store);
    let (win_w, win_h) = initial_settings.window_size();

    let web_context = WebContext::default().expect("failed to get default WebContext");
    register_web_scheme(&web_context, web_dir);

    let panel_window = gtk::ApplicationWindow::new(app);
    panel_window.set_title("KeyCat");
    panel_window.set_decorated(false);
    panel_window.set_resizable(false);
    panel_window.set_skip_taskbar_hint(true);
    panel_window.set_skip_pager_hint(true);
    panel_window.set_size_request(win_w, win_h);
    panel_window.set_default_size(win_w, win_h);

    // Pick initial monitor (saved monitor if still connected, otherwise primary monitor)
    let initial_monitor_index = gdk::Display::default()
        .and_then(|display| {
            let n = display.n_monitors();
            initial_settings.monitor_index.filter(|&i| i >= 0 && i < n)
        })
        .unwrap_or_else(primary_monitor_index);

    // Initialize gtk-layer-shell BEFORE the window is realized!
    let layer_shell_active = gtk_layer_shell::is_supported();
    if layer_shell_active {
        panel_window.init_layer_shell();
        panel_window.set_namespace("keycat");
        panel_window.set_layer(Layer::Top);
        panel_window.set_keyboard_mode(KeyboardMode::None);
        if let Some(mon) = gdk::Display::default().and_then(|d| d.monitor(initial_monitor_index)) {
            panel_window.set_monitor(&mon);
        }
        panel_window.set_anchor(Edge::Right, true);
        panel_window.set_anchor(Edge::Bottom, true);
        panel_window.set_layer_shell_margin(Edge::Right, initial_settings.margin_right);
        panel_window.set_layer_shell_margin(Edge::Bottom, initial_settings.margin_bottom);
        log("gtk-layer-shell initialized on Layer::Top");
    } else {
        panel_window.set_keep_above(true);
        panel_window.set_accept_focus(false);
        log("gtk-layer-shell not supported in current session, falling back to normal window");
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
    }

    panel_window.add(&panel_view);

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
        store,
        page_ready: false,
        hook_ready,
        hook_error,
        layer_shell_active,
        visible: true,
        autostart: autostart::is_enabled(),
        hit: "none".into(),
        petting: false,
        dragging: false,
        drag_grab: None,
        drag_pointer: None,
        pending_drag: (0.0, 0.0),
        monitor_index: initial_monitor_index,
        margin_right: f64::from(initial_settings.margin_right),
        margin_bottom: f64::from(initial_settings.margin_bottom),
        pending_delta: (0.0, 0.0),
        pending_dist: 0.0,
        virtual_cursor: (f64::from(win_w) * 0.5, f64::from(win_h) * 0.5),
        held: HashSet::new(),
        key_events: 0,
        panel_window: panel_window.clone(),
        panel_view: panel_view.clone(),
        settings_window: None,
        context_menu: None,
        tray: None,
        action_tx: action_tx.clone(),
    }));

    // Start KDE native tray
    let initial_snap = state_rc.borrow().snapshot();
    let tray_ctrl = TrayController::start(initial_snap, action_tx.clone());
    state_rc.borrow_mut().tray = Some(tray_ctrl);

    // Handle menu actions from both tray and right-click context menu
    {
        let app_clone = app.clone();
        let state_clone = state_rc.clone();
        let ctx_clone = web_context.clone();
        action_rx.attach(None, move |action| {
            handle_menu_action(&app_clone, &state_clone, &ctx_clone, action);
            glib::ControlFlow::Continue
        });
    }

    // Handle messages from web/index.html (`window.webkit.messageHandlers.cat`)
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
            if let Some(hit) = msg.get("hit").and_then(Value::as_str) {
                st.hit = hit.to_string();
            }
            if msg.get("pointerMove").and_then(Value::as_bool) == Some(true) {
                let x = msg.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = msg.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                st.virtual_cursor = (x, y);
                if st.petting {
                    st.eval_cat(&format!("cat.pet(true, {})", x.round()));
                }
            } else if msg.get("pointerDown").and_then(Value::as_bool) == Some(true) {
                if st.settings().click_through {
                    return;
                }
                let hit = msg
                    .get("hit")
                    .and_then(Value::as_str)
                    .unwrap_or(&st.hit)
                    .to_string();
                let x = msg.get("x").and_then(Value::as_f64).unwrap_or(0.0);
                let y = msg.get("y").and_then(Value::as_f64).unwrap_or(0.0);
                if hit == "pet" {
                    st.petting = true;
                    st.eval_cat(&format!("cat.pet(true, {})", x.round()));
                } else if hit == "drag" {
                    st.dragging = true;
                    st.drag_grab = Some((x, y));
                    st.drag_pointer = None;
                    st.pending_drag = (0.0, 0.0);
                }
            } else if msg.get("pointerUp").and_then(Value::as_bool) == Some(true) {
                if st.petting {
                    st.petting = false;
                    st.eval_cat("cat.pet(false, 0)");
                }
                if st.dragging {
                    st.dragging = false;
                    st.drag_grab = None;
                    st.drag_pointer = None;
                    st.pending_drag = (0.0, 0.0);
                    st.save_margins();
                }
            } else if let Some(ticks) = msg.get("ticks").and_then(Value::as_f64) {
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
        });
    }

    // Track exact in-window pointer coordinates during drag for 1:1 cursor tracking
    // (matches KWin's libinput acceleration and fractional scaling without drift).
    {
        let state_clone = state_rc.clone();
        panel_view.connect_motion_notify_event(move |_, event| {
            let mut st = state_clone.borrow_mut();
            if st.dragging {
                st.drag_pointer = Some(event.position());
            }
            glib::Propagation::Proceed
        });
    }

    // Native GTK right-click handler on WebView so popup_at_pointer carries a valid Wayland serial
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

    // Apply initial options when index.html finishes loading
    {
        let state_clone = state_rc.clone();
        panel_view.connect_load_changed(move |_, event| {
            if event == LoadEvent::Finished {
                let mut st = state_clone.borrow_mut();
                st.page_ready = true;
                st.push_options();
                log("index.html loaded");
            }
        });
    }

    // Apply input region right after window is mapped on Wayland
    {
        let state_clone = state_rc.clone();
        panel_window.connect_map_event(move |_, _| {
            state_clone.borrow().apply_input_region();
            glib::Propagation::Proceed
        });
    }

    // 60fps pump: drain evdev channel and flush mouse movement / eye tracking to Three.js
    {
        let state_clone = state_rc.clone();
        glib::timeout_add_local(Duration::from_millis(16), move || {
            while let Ok(input) = evdev_rx.try_recv() {
                handle_evdev_input(&state_clone, input);
            }
            let mut st = state_clone.borrow_mut();
            if st.dragging {
                // Feedforward (evdev delta) + heavily damped feedback (0.25x in-window error):
                // - Feedforward moves immediately with zero Wayland frame-lag oscillation.
                // - Low-gain feedback smoothly eliminates any speed/scaling drift without overshooting.
                let (mut dx, mut dy) = st.pending_drag;
                st.pending_drag = (0.0, 0.0);
                if let (Some((gx, gy)), Some((px, py))) = (st.drag_grab, st.drag_pointer.take()) {
                    let (ex, ey) = (px - gx, py - gy);
                    if ex.abs() > 2.0 {
                        dx += ex * 0.25;
                    }
                    if ey.abs() > 2.0 {
                        dy += ey * 0.25;
                    }
                }
                if dx.abs() >= 0.5 || dy.abs() >= 0.5 {
                    st.drag_by(dx, dy);
                }
            }
            if st.page_ready && (st.pending_delta != (0.0, 0.0) || st.pending_dist > 0.0) {
                let (mut dx, mut dy) = st.pending_delta;
                let dist = st.pending_dist;
                st.pending_delta = (0.0, 0.0);
                st.pending_dist = 0.0;
                // Preserve total travel distance when rapid back-and-forth shaking cancels vector sum
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
                let (cx, cy) = st.virtual_cursor;
                st.eval_cat(&format!(
                    "cat.mouseMove({dx:.1},{dy:.1});cat.cursor({},{})",
                    cx.round(),
                    cy.round()
                ));
            }
            glib::ControlFlow::Continue
        });
    }

    // Periodic status.txt heartbeat every 15s (same protocol as Windows/macOS builds)
    {
        let state_clone = state_rc.clone();
        glib::timeout_add_local(Duration::from_secs(15), move || {
            state_clone.borrow().eval_cat("cat.state.ticks = 0");
            let state_read = state_clone.clone();
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                state_read.borrow().eval_cat(
                    "window.webkit?.messageHandlers?.cat?.postMessage({ ticks: cat.state.ticks })",
                );
            });
            glib::ControlFlow::Continue
        });
    }

    panel_view.load_uri("keycat://app/index.html");
    panel_window.show_all();
    state_rc.borrow().apply_input_region();
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
            st.panel_window.show_all();
            st.apply_input_region();
            st.sync_tray();
            return;
        }
        // Fallback file lock in case D-Bus single-instance registration is unavailable.
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
