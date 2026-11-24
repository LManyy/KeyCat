//! Global keyboard and mouse input listener via Linux evdev (`/dev/input/event*`).
//! Works natively under Wayland without relying on XWayland/X11, and automatically
//! rescans when devices are plugged in or removed.

use std::collections::HashMap;
use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, EventType, InputEventKind, Key, RelativeAxisType};

use crate::keymap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Right,
    Other,
}

#[derive(Debug)]
pub enum Input {
    Key { id: u16, code: Option<&'static str>, down: bool },
    Button { button: Button, down: bool },
    Move { dx: f64, dy: f64 },
    Wheel(f64),
}

fn is_relevant_device(dev: &Device) -> bool {
    let supported = dev.supported_events();
    let has_keys = supported.contains(EventType::KEY)
        && dev.supported_keys().is_some_and(|keys| {
            keys.contains(Key::KEY_A)
                || keys.contains(Key::KEY_SPACE)
                || keys.contains(Key::KEY_ENTER)
                || keys.contains(Key::BTN_LEFT)
        });
    let has_rel = supported.contains(EventType::RELATIVE)
        && dev.supported_relative_axes().is_some_and(|axes| {
            axes.contains(RelativeAxisType::REL_X)
                || axes.contains(RelativeAxisType::REL_Y)
                || axes.contains(RelativeAxisType::REL_WHEEL)
        });
    has_keys || has_rel
}

fn set_nonblocking(dev: &Device) {
    let fd = dev.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

fn scan_devices(devices: &mut HashMap<PathBuf, Device>) -> (usize, usize) {
    let mut opened = 0usize;
    let mut denied = 0usize;
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("event") || devices.contains_key(&path) {
            continue;
        }
        match Device::open(&path) {
            Ok(dev) => {
                if is_relevant_device(&dev) {
                    set_nonblocking(&dev);
                    devices.insert(path, dev);
                    opened += 1;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                denied += 1;
            }
            Err(_) => {}
        }
    }
    (opened, denied)
}

pub fn start(sink: Sender<Input>) -> Result<usize, String> {
    let mut devices = HashMap::new();
    let (opened, denied) = scan_devices(&mut devices);
    if devices.is_empty() {
        if denied > 0 {
            return Err(
                "无法读取 /dev/input/event*（权限不足，请执行 sudo usermod -aG input $USER 并重新登录）"
                    .into(),
            );
        }
        return Err("未找到可用的键盘或鼠标输入设备".into());
    }

    thread::Builder::new()
        .name("evdev-hook".into())
        .spawn(move || {
            let mut last_scan = Instant::now();
            let mut pollfds: Vec<libc::pollfd> = Vec::new();
            let mut paths: Vec<PathBuf> = Vec::new();

            loop {
                if last_scan.elapsed() >= Duration::from_secs(3) {
                    let _ = scan_devices(&mut devices);
                    last_scan = Instant::now();
                }

                pollfds.clear();
                paths.clear();
                for (path, dev) in &devices {
                    pollfds.push(libc::pollfd {
                        fd: dev.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    });
                    paths.push(path.clone());
                }

                if pollfds.is_empty() {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, 500) };
                if ret <= 0 {
                    continue;
                }

                let mut dead = Vec::new();
                for (idx, pfd) in pollfds.iter().enumerate() {
                    if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                        dead.push(paths[idx].clone());
                        continue;
                    }
                    if pfd.revents & libc::POLLIN == 0 {
                        continue;
                    }
                    let path = &paths[idx];
                    let Some(dev) = devices.get_mut(path) else {
                        continue;
                    };
                    let mut dx = 0.0f64;
                    let mut dy = 0.0f64;
                    match dev.fetch_events() {
                        Ok(events) => {
                            for ev in events {
                                match ev.kind() {
                                    InputEventKind::Key(key) => {
                                        // value: 0 = release, 1 = press, 2 = autorepeat
                                        if ev.value() == 2 {
                                            continue;
                                        }
                                        let down = ev.value() == 1;
                                        let input = match key {
                                            Key::BTN_LEFT => Input::Button { button: Button::Left, down },
                                            Key::BTN_RIGHT => Input::Button { button: Button::Right, down },
                                            Key::BTN_MIDDLE
                                            | Key::BTN_SIDE
                                            | Key::BTN_EXTRA
                                            | Key::BTN_FORWARD
                                            | Key::BTN_BACK => {
                                                Input::Button { button: Button::Other, down }
                                            }
                                            _ => Input::Key {
                                                id: key.code(),
                                                code: keymap::code(key),
                                                down,
                                            },
                                        };
                                        if sink.send(input).is_err() {
                                            return;
                                        }
                                    }
                                    InputEventKind::RelAxis(axis) => match axis {
                                        RelativeAxisType::REL_X => dx += f64::from(ev.value()),
                                        RelativeAxisType::REL_Y => dy += f64::from(ev.value()),
                                        RelativeAxisType::REL_WHEEL
                                            if sink.send(Input::Wheel(f64::from(ev.value()))).is_err() =>
                                        {
                                            return;
                                        }
                                        _ => {}
                                    },
                                    _ => {}
                                }
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            dead.push(path.clone());
                        }
                    }
                    if (dx != 0.0 || dy != 0.0) && sink.send(Input::Move { dx, dy }).is_err() {
                        return;
                    }
                }
                for path in dead {
                    devices.remove(&path);
                }
            }
        })
        .map_err(|err| err.to_string())?;

    Ok(opened)
}
