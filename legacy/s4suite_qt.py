#!/usr/bin/env python3
import sys
import os

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, 
    QVBoxLayout, QHBoxLayout, QLabel, QPushButton, QStackedWidget, QFrame,
    QMessageBox
)
from PyQt6.QtGui import QIcon, QFont
from PyQt6.QtCore import Qt

# Importa o cérebro e temas
from s4common import ConfigManager, command_exists
from s4theme import ThemeManager
from s4translator import t

# Importa as abas
from ui.dashboard_tab import DashboardTab
from ui.installer_tab import InstallerTab
from ui.organizer_tab import OrganizerTab
from ui.merger_tab import MergerTab
from ui.tray_tab import TrayTab
from ui.translations_tab import TranslationsTab
from ui.reshade_tab import ReshadeTab
from ui.config_tab import ConfigTab

def resource_path(relative_path):
    try:
        base_path = sys._MEIPASS
    except Exception:
        base_path = os.path.dirname(__file__)
    return os.path.join(base_path, relative_path)

class S4SuiteMainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        
        self.setWindowIcon(QIcon(resource_path("s4suite.png")))
        
        saved_theme = ConfigManager.get("theme", "Sims Green")
        self.setStyleSheet(ThemeManager.get_stylesheet(saved_theme))
        
        self.setWindowTitle("S4 Suite v2.0 (Premium Edition)")
        self.resize(1150, 800)

        central_widget = QWidget()
        self.setCentralWidget(central_widget)

        # Layout horizontal (Sidebar na esquerda, Conteúdo na direita)
        main_layout = QHBoxLayout(central_widget)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(0)

        # ==========================================
        # BARRA LATERAL (SIDEBAR)
        # ==========================================
        self.sidebar = QFrame()
        self.sidebar.setObjectName("Sidebar")
        self.sidebar.setFixedWidth(240)
        sidebar_layout = QVBoxLayout(self.sidebar)
        sidebar_layout.setContentsMargins(10, 20, 10, 20)
        sidebar_layout.setSpacing(10)

        # Cabeçalho da Sidebar
        lbl_brand = QLabel("S4 SUITE")
        lbl_brand.setFont(QFont("Arial", 22, QFont.Weight.Bold))
        lbl_brand.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lbl_brand.setStyleSheet("color: white; margin-bottom: 20px;")
        sidebar_layout.addWidget(lbl_brand)

        # Botões do Menu
        self.nav_buttons = []
        
        self.btn_home = self.create_nav_button(t("🏠 Início"))
        self.btn_install = self.create_nav_button(t("📦 Instalador"))
        self.btn_org = self.create_nav_button(t("🧹 Organizador"))
        self.btn_merge = self.create_nav_button(t("🔗 Merger"))
        self.btn_tray = self.create_nav_button(t("🗂️ Tray Importer"))
        self.btn_trans = self.create_nav_button(t("🌐 Traduções"))
        self.btn_reshade = self.create_nav_button(t("🔮 ReShade"))
        self.btn_config = self.create_nav_button(t("⚙️ Configurações"))

        sidebar_layout.addWidget(self.btn_home)
        sidebar_layout.addWidget(self.btn_install)
        sidebar_layout.addWidget(self.btn_org)
        sidebar_layout.addWidget(self.btn_merge)
        sidebar_layout.addWidget(self.btn_tray)
        sidebar_layout.addWidget(self.btn_trans)
        sidebar_layout.addWidget(self.btn_reshade)
        
        sidebar_layout.addStretch()
        sidebar_layout.addWidget(self.btn_config)

        main_layout.addWidget(self.sidebar)

        # ==========================================
        # ÁREA DE CONTEÚDO (STACKED WIDGET)
        # ==========================================
        self.content_area = QStackedWidget()
        
        self.dashboard_tab = DashboardTab()
        self.installer_tab = InstallerTab()
        self.organizer_tab = OrganizerTab()
        self.merger_tab = MergerTab()
        self.tray_tab = TrayTab()
        self.translations_tab = TranslationsTab()
        self.reshade_tab = ReshadeTab()
        self.config_tab = ConfigTab()

        self.content_area.addWidget(self.dashboard_tab)
        self.content_area.addWidget(self.installer_tab)
        self.content_area.addWidget(self.organizer_tab)
        self.content_area.addWidget(self.merger_tab)
        self.content_area.addWidget(self.tray_tab)
        self.content_area.addWidget(self.translations_tab)
        self.content_area.addWidget(self.reshade_tab)
        self.content_area.addWidget(self.config_tab)

        # Envolver o stacked_widget para dar uma margem agradável
        content_wrapper = QWidget()
        wrapper_layout = QVBoxLayout(content_wrapper)
        wrapper_layout.setContentsMargins(30, 30, 30, 30)
        wrapper_layout.addWidget(self.content_area)

        main_layout.addWidget(content_wrapper, 1)

        # Conectar botões
        self.btn_home.clicked.connect(lambda: self.switch_page(0, self.btn_home))
        self.btn_install.clicked.connect(lambda: self.switch_page(1, self.btn_install))
        self.btn_org.clicked.connect(lambda: self.switch_page(2, self.btn_org))
        self.btn_merge.clicked.connect(lambda: self.switch_page(3, self.btn_merge))
        self.btn_tray.clicked.connect(lambda: self.switch_page(4, self.btn_tray))
        self.btn_trans.clicked.connect(lambda: self.switch_page(5, self.btn_trans))
        self.btn_reshade.clicked.connect(lambda: self.switch_page(6, self.btn_reshade))
        self.btn_config.clicked.connect(lambda: self.switch_page(7, self.btn_config))

        # Inicia na primeira aba
        self.switch_page(0, self.btn_home)

    def create_nav_button(self, text):
        btn = QPushButton(text)
        btn.setProperty("class", "SidebarBtn")
        self.nav_buttons.append(btn)
        return btn

    def switch_page(self, index, active_btn):
        self.content_area.setCurrentIndex(index)
        for btn in self.nav_buttons:
            btn.setProperty("active", "false")
            btn.style().unpolish(btn)
            btn.style().polish(btn)
            
        active_btn.setProperty("active", "true")
        active_btn.style().unpolish(active_btn)
        active_btn.style().polish(active_btn)

def apply_base_theme(app):
    app.setStyle("Fusion")

if __name__ == "__main__":
    app = QApplication(sys.argv)
    apply_base_theme(app)

    if not command_exists("7z"):
        QMessageBox.warning(
            None,
            t("Dependência ausente"),
            t("O comando '7z' não foi encontrado. Instalação/importação de arquivos .zip/.7z/.rar e ReShade não funcionarão até instalar p7zip/7zip.")
        )
    
    window = S4SuiteMainWindow()
    window.show()
    
    sys.exit(app.exec())
