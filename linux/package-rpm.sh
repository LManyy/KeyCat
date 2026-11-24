#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LINUX_DIR="$ROOT_DIR/linux"
DIST_DIR="$ROOT_DIR/dist"

if ! command -v rpmbuild >/dev/null 2>&1; then
    echo "错误：未找到 rpmbuild，请先安装：sudo dnf install rpm-build" >&2
    exit 1
fi

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$LINUX_DIR/Cargo.toml" | head -n 1)"
if [[ -z "$VERSION" ]]; then
    echo "错误：无法从 linux/Cargo.toml 解析版本号" >&2
    exit 1
fi

echo "==> 编译 KeyCat v$VERSION (release)..."
(cd "$LINUX_DIR" && cargo build --release)

TOPDIR="$(mktemp -d)"
trap 'rm -rf "$TOPDIR"' EXIT

mkdir -p "$TOPDIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS} "$DIST_DIR"
cp "$LINUX_DIR/target/release/keycat" "$TOPDIR/SOURCES/keycat"
cp -r "$ROOT_DIR/web" "$TOPDIR/SOURCES/web"
cp "$LINUX_DIR/packaging/keycat.desktop" "$TOPDIR/SOURCES/keycat.desktop"
cp "$LINUX_DIR/icons/icon.png" "$TOPDIR/SOURCES/keycat.png"
cp "$LINUX_DIR/packaging/keycat.spec" "$TOPDIR/SPECS/keycat.spec"

echo "==> 正在构建 RPM 安装包..."
rpmbuild -bb \
    --define "_topdir $TOPDIR" \
    --define "pkg_version $VERSION" \
    "$TOPDIR/SPECS/keycat.spec"

RPM_FILE="$(find "$TOPDIR/RPMS" -name '*.rpm' | head -n 1)"
if [[ -z "$RPM_FILE" ]]; then
    echo "错误：未找到生成的 RPM 文件" >&2
    exit 1
fi

cp -f "$RPM_FILE" "$DIST_DIR/"
OUT_PATH="$DIST_DIR/$(basename "$RPM_FILE")"
echo "==> 打包完成：$OUT_PATH"
echo "安装命令：sudo dnf install \"$OUT_PATH\""
