//! XDG Autostart (`~/.config/autostart/keycat.desktop`).

use std::fs;
use std::io;
use std::path::PathBuf;

fn desktop_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("autostart").join("keycat.desktop"))
}

pub fn is_enabled() -> bool {
    desktop_path().is_some_and(|p| p.exists())
}

pub fn set_enabled(enable: bool) -> io::Result<()> {
    let path = desktop_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定 ~/.config 目录"))?;
    if !enable {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        }
    }
    let exe = std::env::current_exe()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=KeyCat\n\
         Comment=Desktop typing cat\n\
         Exec=\"{}\"\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    fs::write(path, content)
}
