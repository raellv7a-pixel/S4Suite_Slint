#!/bin/bash
set -e

echo "Criando Ambiente Virtual (PEP 668 Bypass)..."
rm -rf build_env
python3 -m venv build_env
source build_env/bin/activate

echo "Instalando dependências no VENV..."
pip install -r requirements-build.txt

echo "Limpando builds antigos..."
rm -rf build dist S4Suite.AppDir S4Suite_v2.0-x86_64.AppImage

echo "Compilando com PyInstaller (Isso pode demorar um pouco)..."
pyinstaller --noconsole --name S4Suite --add-data "s4suite.png:." --add-data "reshade.exe:." --add-data "locales:locales" s4suite_qt.py

echo "Criando estrutura AppDir..."
mkdir -p S4Suite.AppDir/usr/bin
mkdir -p S4Suite.AppDir/usr/share/applications
mkdir -p S4Suite.AppDir/usr/share/icons/hicolor/256x256/apps

# Copiando binário
cp -r dist/S4Suite/* S4Suite.AppDir/usr/bin/

echo "Removendo bibliotecas do sistema conflitantes..."
rm -f S4Suite.AppDir/usr/bin/_internal/libstdc++.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libgcc_s.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libglib-2.0.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libgthread-2.0.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libgobject-2.0.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libgio-2.0.so*
rm -f S4Suite.AppDir/usr/bin/_internal/libz.so*

# Copiando ícones
cp s4suite.png S4Suite.AppDir/s4suite.png
cp s4suite.png S4Suite.AppDir/.DirIcon
cp s4suite.png S4Suite.AppDir/usr/share/icons/hicolor/256x256/apps/

echo "Criando S4Suite.desktop..."
cat << 'EOF' > S4Suite.AppDir/S4Suite.desktop
[Desktop Entry]
Name=S4 Suite
Comment=Mod Manager for The Sims 4
Exec=AppRun
Icon=s4suite
Terminal=false
Type=Application
Categories=Utility;Game;
EOF

echo "Criando AppRun..."
cat << 'EOF' > S4Suite.AppDir/AppRun
#!/bin/sh
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/bin:${LD_LIBRARY_PATH}"
exec "${HERE}/usr/bin/S4Suite" "$@"
EOF

chmod +x S4Suite.AppDir/AppRun

echo "Baixando appimagetool..."
if [ ! -f "appimagetool-x86_64.AppImage" ]; then
    wget -q https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x appimagetool-x86_64.AppImage
fi

echo "Gerando o AppImage final..."
ARCH=x86_64 ./appimagetool-x86_64.AppImage S4Suite.AppDir S4Suite_v2.0-x86_64.AppImage

echo "✅ AppImage criado com sucesso: S4Suite_v2.0-x86_64.AppImage"
