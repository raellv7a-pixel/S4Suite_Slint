#!/usr/bin/env bash
#
# Gera o AppImage da S4Suite a partir do binário Rust.
#
# O empacotamento do app PyQt (legacy/build.sh) precisava de venv, PyInstaller,
# e de apagar à mão as libs do sistema que o bundle duplicava. Nada disso se
# aplica aqui: o binário Rust já carrega os locales embutidos (rust-embed) e
# linka contra a libc do sistema.

set -euo pipefail

APP_NAME="S4Suite"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
ARCH="${ARCH:-x86_64}"
APPDIR="target/${APP_NAME}.AppDir"
OUTPUT="${APP_NAME}_v${VERSION}-${ARCH}.AppImage"

cd "$(dirname "$0")"

echo "==> Compilando em release..."
cargo build --release

echo "==> Montando ${APPDIR}..."
rm -rf "${APPDIR}" "${OUTPUT}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"

cp "target/release/s4suite" "${APPDIR}/usr/bin/${APP_NAME}"

# O ícone precisa estar em três lugares: a raiz do AppDir e o .DirIcon são o que
# o lançador lê, e o caminho hicolor é o que vale depois de instalado.
cp assets/s4suite.png "${APPDIR}/s4suite.png"
cp assets/s4suite.png "${APPDIR}/.DirIcon"
cp assets/s4suite.png "${APPDIR}/usr/share/icons/hicolor/256x256/apps/s4suite.png"

cat > "${APPDIR}/${APP_NAME}.desktop" <<'DESKTOP'
[Desktop Entry]
Name=S4 Suite
Comment=Gerenciador de mods e conteúdo para The Sims 4
Exec=AppRun
Icon=s4suite
Terminal=false
Type=Application
Categories=Utility;Game;
DESKTOP
cp "${APPDIR}/${APP_NAME}.desktop" "${APPDIR}/usr/share/applications/"

cat > "${APPDIR}/AppRun" <<APPRUN
#!/bin/sh
HERE="\$(dirname "\$(readlink -f "\${0}")")"
export PATH="\${HERE}/usr/bin:\${PATH}"
exec "\${HERE}/usr/bin/${APP_NAME}" "\$@"
APPRUN
chmod +x "${APPDIR}/AppRun"

TOOL="target/appimagetool-${ARCH}.AppImage"
if [ ! -f "${TOOL}" ]; then
    echo "==> Baixando appimagetool..."
    curl -fsSL -o "${TOOL}" \
        "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${ARCH}.AppImage"
    chmod +x "${TOOL}"
fi

echo "==> Gerando ${OUTPUT}..."
ARCH="${ARCH}" "${TOOL}" "${APPDIR}" "${OUTPUT}"

echo "✅ ${OUTPUT}"
