from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QLineEdit, 
    QPushButton, QFileDialog, QGroupBox, QListWidget, QListWidgetItem, QMessageBox
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal
from s4common import get_mods_dir, safe_delete_folder, safe_remove
from s4installer import install_mod
import os
import shutil
from s4translator import t

class TranslationWorker(QThread):
    finished_signal = pyqtSignal(bool, str)

    def __init__(self, path, trans_dir):
        super().__init__()
        self.path = path
        self.trans_dir = trans_dir

    def run(self):
        try:
            status, msg = install_mod(self.path, self.trans_dir)
            if status is True:
                self.finished_signal.emit(True, t("Tradução instalada com sucesso!"))
            else:
                self.finished_signal.emit(False, msg or t("Falha ao instalar a tradução."))
        except Exception as e:
            self.finished_signal.emit(False, str(e))

class TranslationsTab(QWidget):
    def __init__(self):
        super().__init__()
        self.setAcceptDrops(True)
        self.build_ui()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.accept()
        else:
            event.ignore()

    def dropEvent(self, event):
        files = [u.toLocalFile() for u in event.mimeData().urls()]
        valid_files = [f for f in files if f.lower().endswith(('.zip', '.7z', '.rar', '.package'))]
        if valid_files:
            self.file_entry.setText(valid_files[0])

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(15)

        title = QLabel(t("🌐 Gerenciador de Traduções"))
        title.setStyleSheet("font-size: 18px; font-weight: bold; ")
        layout.addWidget(title)

        desc = QLabel(t("Selecione um arquivo de tradução para injetar na pasta 01_Traducoes."))
        desc.setStyleSheet("")
        layout.addWidget(desc)

        # File Select Row
        group_files = QGroupBox(t("Instalação de Tradução (.package, .zip)"))
        layout_files = QHBoxLayout(group_files)

        self.file_entry = QLineEdit()
        self.file_entry.setPlaceholderText(t("Nenhum arquivo selecionado..."))
        self.file_entry.setReadOnly(True)
        layout_files.addWidget(self.file_entry)

        btn_browse = QPushButton(t("📁 Procurar"))
        btn_browse.clicked.connect(self.on_browse_clicked)
        layout_files.addWidget(btn_browse)

        layout.addWidget(group_files)

        # Install Button
        self.btn_install = QPushButton(t("📥 Instalar/Atualizar Tradução"))
        self.btn_install.setObjectName("primary")
        self.btn_install.clicked.connect(self.on_install_clicked)
        layout.addWidget(self.btn_install)

        # Installed List
        lbl_list = QLabel(t("Traduções Instaladas:"))
        lbl_list.setStyleSheet("font-weight: bold; margin-top: 10px;")
        layout.addWidget(lbl_list)

        self.list_widget = QListWidget()
        
        layout.addWidget(self.list_widget)

        self.load_translations()

    def on_browse_clicked(self):
        downloads_dir = os.path.expanduser("~/Downloads")
        file, _ = QFileDialog.getOpenFileName(
            self, 
            t("Selecione o Arquivo de Tradução"), 
            downloads_dir, 
            t("Mods e Zips (*.package *.zip *.7z *.rar);;Todos (*.*)"),
            options=QFileDialog.Option.DontUseNativeDialog
        )
        if file:
            self.file_entry.setText(file)

    def load_translations(self):
        self.list_widget.clear()
        
        mods_dir = get_mods_dir()
        if not mods_dir: return
        trans_dir = os.path.join(mods_dir, "01_Traducoes")
        if not os.path.exists(trans_dir): return

        for f in os.listdir(trans_dir):
            item = QListWidgetItem()
            widget = QWidget()
            row = QHBoxLayout(widget)
            row.setContentsMargins(0, 0, 0, 0)
            
            safe_f = f.encode('utf-8', 'replace').decode('utf-8')
            lbl_name = QLabel(safe_f)
            row.addWidget(lbl_name)
            
            btn_del = QPushButton(t("🗑️ Excluir"))
            btn_del.setObjectName("danger")
            btn_del.setFixedWidth(120)
            
            # Using default arguments in lambda to capture the current file path
            path = os.path.join(trans_dir, f)
            btn_del.clicked.connect(lambda checked, p=path: self.on_delete_trans(p))
            
            row.addWidget(btn_del)
            
            item.setSizeHint(widget.sizeHint())
            self.list_widget.addItem(item)
            self.list_widget.setItemWidget(item, widget)

    def on_delete_trans(self, path):
        resposta = QMessageBox.question(
            self, t("Confirmar"), t("Tem certeza que deseja excluir esta tradução?\n{}").format(os.path.basename(path)),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No
        )
        if resposta == QMessageBox.StandardButton.Yes:
            try:
                if os.path.isdir(path):
                    safe_delete_folder(path)
                else:
                    safe_remove(path)
                self.load_translations()
            except Exception as e:
                QMessageBox.critical(self, t("Erro"), t("Erro ao excluir: {}").format(e))

    def on_install_clicked(self):
        path = self.file_entry.text()
        mods_dir = get_mods_dir()
        
        if not path or not mods_dir:
            QMessageBox.warning(self, t("Aviso"), t("Selecione o arquivo e configure a pasta do jogo."))
            return
        
        trans_dir = os.path.join(mods_dir, "01_Traducoes")
        if not os.path.exists(trans_dir):
            os.makedirs(trans_dir)
        
        self.btn_install.setEnabled(False)
        self.btn_install.setText(t("Instalando..."))
        
        self.worker = TranslationWorker(path, trans_dir)
        self.worker.finished_signal.connect(self.on_install_finished)
        self.worker.start()

    def on_install_finished(self, success, msg):
        self.btn_install.setEnabled(True)
        self.btn_install.setText(t("📥 Instalar/Atualizar Tradução"))
        
        if success:
            QMessageBox.information(self, t("Sucesso"), msg)
            self.file_entry.clear()
            self.load_translations()
        else:
            QMessageBox.critical(self, t("Erro"), msg)
