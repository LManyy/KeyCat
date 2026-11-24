Name:           keycat
Version:        %{pkg_version}
Release:        1%{?dist}
Summary:        A desktop cat living on your keyboard (Fedora KDE Wayland)
License:        MIT
URL:            https://github.com/ElloboDK/KeyCat

Requires:       gtk3
Requires:       webkit2gtk4.1
Requires:       gtk-layer-shell

# Pre-built release binary is staged by package-rpm.sh
%global debug_package %{nil}

%description
KeyCat is a lightweight desktop typing cat for Fedora KDE Wayland.
It renders a transparent 3D/2D cat overlay via GTK3, gtk-layer-shell and
WebKitGTK, tracks global keyboard and mouse input natively through evdev,
and integrates with KDE Plasma's native StatusNotifierItem system tray.

%install
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}%{_datadir}/keycat/web
mkdir -p %{buildroot}%{_datadir}/applications
mkdir -p %{buildroot}%{_datadir}/icons/hicolor/512x512/apps

install -m 0755 %{_sourcedir}/keycat %{buildroot}%{_bindir}/keycat
cp -r %{_sourcedir}/web/* %{buildroot}%{_datadir}/keycat/web/
install -m 0644 %{_sourcedir}/keycat.desktop %{buildroot}%{_datadir}/applications/keycat.desktop
install -m 0644 %{_sourcedir}/keycat.png %{buildroot}%{_datadir}/icons/hicolor/512x512/apps/keycat.png

%post
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q %{_datadir}/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t -q %{_datadir}/icons/hicolor || :
fi
cat <<'EOF'
KeyCat installed.
To enable global keyboard and mouse capture under Wayland, add your user to the 'input' group:
  sudo usermod -aG input "$USER"
(Log out and log back in for the change to take effect.)

[SECURITY WARNING]
Adding your account to the 'input' group allows ANY non-sandboxed process running under your
user to read /dev/input/event* directly (bypassing Wayland input isolation, including keystrokes
typed into password fields). Only enable this on trusted personal machines.

To revoke this permission at any time:
  sudo gpasswd -d "$USER" input
(Log out and log back in to take effect.)
EOF

%postun
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q %{_datadir}/applications || :
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t -q %{_datadir}/icons/hicolor || :
fi

%files
%{_bindir}/keycat
%{_datadir}/keycat/web/
%{_datadir}/applications/keycat.desktop
%{_datadir}/icons/hicolor/512x512/apps/keycat.png
