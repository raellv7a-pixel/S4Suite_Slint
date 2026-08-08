from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QLineEdit, 
    QPushButton, QFileDialog, QGroupBox, QTextEdit, QMessageBox, QComboBox
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal
from s4common import ConfigManager
import s4reshade
import os
from s4translator import t

class ReshadeInstallWorker(QThread):
    log_signal = pyqtSignal(str)
    finished_signal = pyqtSignal(bool)
    
    def __init__(self, game_bin_path, inject_name="dxgi.dll"):
        super().__init__()
        self.game_bin_path = game_bin_path
        self.inject_name = inject_name
        
    def run(self):
        self.log_signal.emit(t("⏳ Iniciando a Instalação Automática do ReShade..."))
        success, msg = s4reshade.install_reshade(self.game_bin_path, self.inject_name, lambda m: self.log_signal.emit(m))
        self.log_signal.emit(msg)
        self.finished_signal.emit(success)

class PresetInstallWorker(QThread):
    log_signal = pyqtSignal(str)
    finished_signal = pyqtSignal(bool)
    
    def __init__(self, preset_path, game_bin_path):
        super().__init__()
        self.preset_path = preset_path
        self.game_bin_path = game_bin_path
        
    def run(self):
        self.log_signal.emit(t("⏳ Instalando Preset: {}").format(os.path.basename(self.preset_path)))
        success, msg = s4reshade.install_preset(self.preset_path, self.game_bin_path, lambda m: self.log_signal.emit(m))
        self.log_signal.emit(f"{'✅' if success else '❌'} {msg}")
        self.finished_signal.emit(success)


class ReshadeTab(QWidget):
    def __init__(self):
        super().__init__()
        self.build_ui()

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(15)

        title = QLabel(t("🔮 Gerenciador ReShade (Linux Edition)"))
        title.setObjectName("HeaderTitle")
        layout.addWidget(title)

        desc = QLabel(t("Instale e desinstale o ReShade (DX11) com um clique e adicione seus Presets (.zip) favoritos sem confusão de diretórios."))
        desc.setObjectName("SubDescription")
        layout.addWidget(desc)

        # Pasta Game/Bin
        group_path = QGroupBox(t("Pasta Game/Bin"))
        layout_path = QHBoxLayout(group_path)
        
        self.entry_path = QLineEdit()
        self.entry_path.setPlaceholderText(t("Selecione a pasta onde fica o TS4_x64.exe..."))
        # Auto-detect if we have sims4_path
        sims_path = ConfigManager.get("sims4_path")
        if sims_path:
            possible_bin = os.path.join(sims_path, "Game", "Bin")
            if os.path.exists(possible_bin):
                self.entry_path.setText(possible_bin)
        
        layout_path.addWidget(self.entry_path)
        
        btn_browse = QPushButton(t("📁 Procurar"))
        btn_browse.clicked.connect(self.on_browse_bin)
        layout_path.addWidget(btn_browse)
        
        layout.addWidget(group_path)

        # Controles ReShade
        row_reshade = QHBoxLayout()
        
        self.combo_mode = QComboBox()
        self.combo_mode.addItems([t("Modo Padrão (dxgi.dll)"), t("Modo Faugus / Lutris (d3d11.dll)")])
        self.combo_mode.setToolTip(t("Use o Modo Faugus para evitar conflitos de tela preta com o DXVK no Linux."))
        row_reshade.addWidget(self.combo_mode)
        
        self.btn_install_rs = QPushButton(t("✨ Instalar ReShade"))
        self.btn_install_rs.setObjectName("primary")
        self.btn_install_rs.clicked.connect(self.on_install_reshade)
        row_reshade.addWidget(self.btn_install_rs)
        
        self.btn_uninstall_rs = QPushButton(t("🗑️ Desinstalar ReShade"))
        self.btn_uninstall_rs.setObjectName("danger")
        self.btn_uninstall_rs.clicked.connect(self.on_uninstall_reshade)
        row_reshade.addWidget(self.btn_uninstall_rs)
        
        layout.addLayout(row_reshade)

        # Presets
        group_presets = QGroupBox(t("Instalar Preset de Gráficos (.zip)"))
        layout_presets = QHBoxLayout(group_presets)
        
        self.btn_preset = QPushButton(t("📦 Selecionar e Instalar Preset"))
        self.btn_preset.clicked.connect(self.on_install_preset)
        layout_presets.addWidget(self.btn_preset)
        
        layout.addWidget(group_presets)

        # Caixa de Aviso Linux (Clipboard)
        group_linux = QGroupBox(t("Obrigatório para Usuários de Linux / Steam Deck"))
        group_linux.setStyleSheet("QGroupBox { background-color: rgba(217, 119, 54, 0.1); border: 1px solid #d97736; } QGroupBox::title { color: #d97736; }")
        layout_linux = QVBoxLayout(group_linux)
        
        lbl_linux = QLabel(t("Para o ReShade funcionar no Linux, o jogo precisa carregar o arquivo dxgi.dll falso em vez do original do Windows. Adicione a linha abaixo nas opções de inicialização do The Sims 4 (na Steam ou EA App via Lutris):"))
        lbl_linux.setWordWrap(True)
        layout_linux.addWidget(lbl_linux)
        
        row_copy = QHBoxLayout()
        self.entry_code = QLineEdit("WINEDLLOVERRIDES=\"dxgi=n,b\" %command%")
        self.entry_code.setReadOnly(True)
        self.entry_code.setStyleSheet("font-family: monospace; font-size: 14px;")
        row_copy.addWidget(self.entry_code)
        
        btn_copy = QPushButton(t("📋 Copiar Comando"))
        btn_copy.clicked.connect(self.on_copy_command)
        row_copy.addWidget(btn_copy)
        
        layout_linux.addLayout(row_copy)
        layout.addWidget(group_linux)

        # Log Console
        self.log_console = QTextEdit()
        self.log_console.setReadOnly(True)
        layout.addWidget(self.log_console)

    def append_log(self, text):
        self.log_console.append(text)
        scrollbar = self.log_console.verticalScrollBar()
        scrollbar.setValue(scrollbar.maximum())

    def on_browse_bin(self):
        start_dir = ConfigManager.get("sims4_path") or os.path.expanduser("~")
        folder = QFileDialog.getExistingDirectory(self, t("Selecione a pasta Game/Bin do The Sims 4"), start_dir, options=QFileDialog.Option.DontUseNativeDialog)
        if folder:
            self.entry_path.setText(folder)

    def on_copy_command(self):
        from PyQt6.QtWidgets import QApplication
        QApplication.clipboard().setText(self.entry_code.text())
        QMessageBox.information(self, t("Copiado"), t("O comando foi copiado para a Área de Transferência!\n\nCole nas 'Opções de Inicialização' do The Sims 4 na Steam ou Lutris."))

    def on_install_reshade(self):
        game_bin = self.entry_path.text()
        if not os.path.exists(os.path.join(game_bin, "TS4_x64.exe")):
            QMessageBox.warning(self, t("Erro"), t("TS4_x64.exe não encontrado na pasta informada. Selecione a pasta Game/Bin."))
            return
            
        inject_name = "d3d11.dll" if "Faugus" in self.combo_mode.currentText() else "dxgi.dll"
            
        self.log_console.clear()
        self.btn_install_rs.setEnabled(False)
        self.btn_uninstall_rs.setEnabled(False)
        self.rs_worker = ReshadeInstallWorker(game_bin, inject_name)
        self.rs_worker.log_signal.connect(self.append_log)
        self.rs_worker.finished_signal.connect(self.on_reshade_install_finished)
        self.rs_worker.start()

    def on_reshade_install_finished(self, success):
        self.btn_install_rs.setEnabled(True)
        self.btn_uninstall_rs.setEnabled(True)
        if success:
            QMessageBox.information(self, t("Sucesso"), t("ReShade instalado com sucesso."))
        else:
            QMessageBox.critical(self, t("Erro"), t("A instalação do ReShade falhou. Veja o log para detalhes."))

    def on_uninstall_reshade(self):
        game_bin = self.entry_path.text()
        if not game_bin: return
        
        reply = QMessageBox.question(self, t("Confirmar"), t("Tem certeza que deseja remover o ReShade e todos os seus presets do jogo?"), QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No)
        if reply == QMessageBox.StandardButton.Yes:
            success, msg = s4reshade.uninstall_reshade(game_bin)
            self.append_log(f"🗑️ {msg}")

    def on_install_preset(self):
        game_bin = self.entry_path.text()
        if not os.path.exists(os.path.join(game_bin, "dxgi.dll")) and not os.path.exists(os.path.join(game_bin, "d3d11.dll")):
            QMessageBox.warning(self, t("Erro"), t("O ReShade não parece estar instalado. Instale-o primeiro."))
            return
            
        file_path, _ = QFileDialog.getOpenFileName(self, t("Selecionar Preset"), os.path.expanduser("~/Downloads"), t("Arquivos ZIP (*.zip);;Todos (*.*)"), options=QFileDialog.Option.DontUseNativeDialog)
        if file_path:
            self.btn_preset.setEnabled(False)
            self.pre_worker = PresetInstallWorker(file_path, game_bin)
            self.pre_worker.log_signal.connect(self.append_log)
            self.pre_worker.finished_signal.connect(self.on_preset_install_finished)
            self.pre_worker.start()

    def on_preset_install_finished(self, success):
        self.btn_preset.setEnabled(True)
        if success:
            QMessageBox.information(self, t("Sucesso"), t("Preset instalado com sucesso."))
        else:
            QMessageBox.critical(self, t("Erro"), t("A instalação do preset falhou. Veja o log para detalhes."))
