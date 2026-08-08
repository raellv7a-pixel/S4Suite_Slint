from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QLineEdit, 
    QPushButton, QFileDialog, QGroupBox, QComboBox, QMessageBox, QScrollArea
)
from PyQt6.QtCore import Qt
from s4common import ConfigManager, get_documents_dir
from s4translator import t
import os

class ConfigTab(QWidget):
    def __init__(self):
        super().__init__()
        self.build_ui()

    def build_ui(self):
        main_layout = QVBoxLayout(self)
        
        # Scroll Area para acomodar muitas configurações
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QScrollArea.Shape.NoFrame)
        
        container = QWidget()
        layout = QVBoxLayout(container)
        layout.setSpacing(20)

        # 1. Grupo de Configuração do Jogo
        group_game = QGroupBox(t("Caminho do Jogo"))
        layout_game = QVBoxLayout(group_game)
        
        lbl_game_desc = QLabel(t("O app precisa saber onde o The Sims 4 está instalado para gerenciar seus mods."))
        lbl_game_desc.setStyleSheet("font-size: 12px;")
        layout_game.addWidget(lbl_game_desc)

        lbl_sims4 = QLabel(t("Pasta Raiz do Jogo:"))
        layout_game.addWidget(lbl_sims4)

        row_path = QHBoxLayout()
        self.entry_path = QLineEdit()
        self.entry_path.setPlaceholderText(t("Ex: /home/user/Games/The Sims 4"))
        saved_path = ConfigManager.get("sims4_path") or ""
        self.entry_path.setText(saved_path)
        row_path.addWidget(self.entry_path)

        btn_browse = QPushButton(t("📁 Procurar"))
        btn_browse.setStyleSheet("padding: 5px 15px; font-weight: bold;")
        btn_browse.clicked.connect(self.on_browse_game)
        row_path.addWidget(btn_browse)
        
        btn_autodetect = QPushButton(t("🔍 Auto-Detectar"))
        btn_autodetect.setStyleSheet("padding: 5px 15px; font-weight: bold; background-color: #2a82da; color: white;")
        btn_autodetect.clicked.connect(self.on_autodetect)
        row_path.addWidget(btn_autodetect)

        layout_game.addLayout(row_path)
        layout.addWidget(group_game)

        # 2. Grupo de Opções do Motor (Merger)
        group_merger = QGroupBox(t("Opções do Motor (Merger)"))
        layout_merger = QVBoxLayout(group_merger)
        
        lbl_merger_desc = QLabel(t("Tamanhos maiores geram menos arquivos, mas usam mais RAM do jogo."))
        lbl_merger_desc.setStyleSheet("font-size: 12px;")
        layout_merger.addWidget(lbl_merger_desc)

        row_merger = QHBoxLayout()
        lbl_limit = QLabel(t("Tamanho Limite do Package:"))
        row_merger.addWidget(lbl_limit)

        self.merge_limit_combo = QComboBox()
        self.merge_limit_combo.addItems([t("Sempre perguntar"), "1.0 GB", "1.5 GB", "1.85 GB"])
        
        saved_limit = ConfigManager.get("merge_limit", "ask")
        if saved_limit == 1.0: self.merge_limit_combo.setCurrentIndex(1)
        elif saved_limit == 1.5: self.merge_limit_combo.setCurrentIndex(2)
        elif saved_limit == 1.85: self.merge_limit_combo.setCurrentIndex(3)
        else: self.merge_limit_combo.setCurrentIndex(0)
        
        row_merger.addWidget(self.merge_limit_combo)
        row_merger.addStretch()
        
        layout_merger.addLayout(row_merger)
        layout.addWidget(group_merger)

        # 3. Grupo de Personalização (Tema)
        group_theme = QGroupBox(t("Personalização Visual"))
        layout_theme = QVBoxLayout(group_theme)
        
        lbl_theme_desc = QLabel(t("Escolha as cores do aplicativo."))
        lbl_theme_desc.setStyleSheet("font-size: 12px;")
        layout_theme.addWidget(lbl_theme_desc)
        
        row_theme = QHBoxLayout()
        lbl_theme = QLabel(t("Tema do Aplicativo:"))
        row_theme.addWidget(lbl_theme)
        
        self.theme_combo = QComboBox()
        from s4theme import ThemeManager
        self.theme_combo.addItems(list(ThemeManager.THEMES.keys()))
        saved_theme = ConfigManager.get("theme", "Sims Green")
        self.theme_combo.setCurrentText(saved_theme)
        row_theme.addWidget(self.theme_combo)
        row_theme.addStretch()
        
        layout_theme.addLayout(row_theme)
        layout.addWidget(group_theme)

        # 4. Grupo de Idioma
        group_lang = QGroupBox(t("Idioma"))
        layout_lang = QVBoxLayout(group_lang)
        
        lbl_lang_desc = QLabel(t("Escolha o idioma do aplicativo (requer reinício)."))
        lbl_lang_desc.setStyleSheet("font-size: 12px;")
        layout_lang.addWidget(lbl_lang_desc)
        
        row_lang = QHBoxLayout()
        lbl_lang = QLabel(t("Idioma:"))
        row_lang.addWidget(lbl_lang)
        
        self.lang_combo = QComboBox()
        self.lang_combo.addItems(["Português", "English", "Español"])
        saved_lang = ConfigManager.get("language", "Português")
        self.lang_combo.setCurrentText(saved_lang)
        row_lang.addWidget(self.lang_combo)
        row_lang.addStretch()
        
        layout_lang.addLayout(row_lang)
        layout.addWidget(group_lang)

        layout.addStretch()
        scroll.setWidget(container)
        main_layout.addWidget(scroll)

        # Botão de Salvar Tudo (Fixo no rodapé)
        self.btn_save = QPushButton(t("💾 Salvar Todas as Configurações"))
        self.btn_save.setObjectName("primary")
        self.btn_save.clicked.connect(self.on_save_clicked)
        main_layout.addWidget(self.btn_save)

    def on_browse_game(self):
        start_dir = get_documents_dir()
        folder = QFileDialog.getExistingDirectory(self, t("Selecione a Pasta Raiz do The Sims 4"), start_dir, options=QFileDialog.Option.DontUseNativeDialog)
        if folder:
            self.entry_path.setText(folder)

    def on_autodetect(self):
        import os, glob
        home = os.path.expanduser("~")
        
        possible_paths = [
            # Faugus Launcher
            os.path.join(home, "Faugus/the-sims-4/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            # Steam Flatpak
            os.path.join(home, ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            # Steam Nativo
            os.path.join(home, ".steam/steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            os.path.join(home, ".local/share/Steam/steamapps/compatdata/1222670/pfx/drive_c/users/steamuser/Documents/Electronic Arts/The Sims 4"),
            # Documentos Nativo / Wine sem prefixo
            os.path.join(home, "Documents/Electronic Arts/The Sims 4"),
            os.path.join(home, "Documentos/Electronic Arts/The Sims 4")
        ]
        
        # Heroic Launcher
        heroic_pattern = os.path.join(home, "Games/Heroic/Prefixes/*/drive_c/users/*/Documents/Electronic Arts/The Sims 4")
        possible_paths.extend(glob.glob(heroic_pattern))
        
        # Lutris/Wine Generico
        wine_pattern = os.path.join(home, ".wine/drive_c/users/*/Documents/Electronic Arts/The Sims 4")
        possible_paths.extend(glob.glob(wine_pattern))
        
        found_path = None
        for path in possible_paths:
            if os.path.exists(path) and os.path.isdir(path):
                found_path = path
                break
                
        if found_path:
            self.entry_path.setText(found_path)
            msg = t("Caminho encontrado automaticamente:\n{found_path}").format(found_path=found_path)
            QMessageBox.information(self, t("Detecção"), msg)
        else:
            QMessageBox.warning(self, t("Falha"), t("Não foi possível localizar a pasta do The Sims 4. Selecione manualmente."))

    def on_save_clicked(self):
        path = self.entry_path.text().strip()

        if path and not os.path.isdir(path):
            QMessageBox.warning(self, t("Caminho inválido"), t("A pasta raiz do The Sims 4 precisa existir:\n{}").format(path))
            return
        
        idx = self.merge_limit_combo.currentIndex()
        limits = ["ask", 1.0, 1.5, 1.85]

        config = ConfigManager.load()
        config["sims4_path"] = path
        config.pop("categories", None)
        config["merge_limit"] = limits[idx]
        config["theme"] = self.theme_combo.currentText()
        config["language"] = self.lang_combo.currentText()

        if not ConfigManager.save(config):
            QMessageBox.critical(self, t("Erro"), t("Não foi possível salvar as configurações."))
            return
        
        QMessageBox.information(self, t("Sucesso"), t("✅ Configurações salvas! Pode ser necessário reiniciar o app para aplicar algumas alterações em 100% das telas."))
