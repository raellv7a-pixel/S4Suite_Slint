from PyQt6.QtWidgets import (
    QApplication, QWidget, QVBoxLayout, QHBoxLayout, QLabel, QPushButton, 
    QFrame, QGridLayout, QDialog, QListWidget, QListWidgetItem, QMessageBox
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal
from s4common import get_documents_dir, get_mods_dir, get_tray_dir, ConfigManager, safe_remove, safe_delete_folder, unique_dest_path
import os
import shutil
import datetime
import tempfile
from s4translator import t

def format_size(size_in_bytes):
    if size_in_bytes < 1024:
        return f"{size_in_bytes} B"
    elif size_in_bytes < 1024 * 1024:
        return f"{size_in_bytes / 1024:.2f} KB"
    elif size_in_bytes < 1024 * 1024 * 1024:
        return f"{size_in_bytes / (1024 * 1024):.2f} MB"
    else:
        return f"{size_in_bytes / (1024 * 1024 * 1024):.2f} GB"

class ClearCacheWorker(QThread):
    finished_signal = pyqtSignal(bool, str)
    def run(self):
        sims_path = ConfigManager.get("sims4_path")
        if not sims_path or not os.path.exists(sims_path):
            self.finished_signal.emit(False, t("Pasta do The Sims 4 não configurada."))
            return
            
        try:
            removed = 0
            failed = []
            files_to_remove = ["localthumbcache.package", "avatarcache.package", "spotlight_pt-br.package"]
            folders_to_empty = ["cache", "cachestr", "onlinethumbnailcache"]
            
            for f in files_to_remove:
                path = os.path.join(sims_path, f)
                if os.path.exists(path):
                    if safe_remove(path):
                        removed += 1
                    else:
                        failed.append(path)
                
            for d in folders_to_empty:
                folder_path = os.path.join(sims_path, d)
                if os.path.exists(folder_path):
                    for item in os.listdir(folder_path):
                        item_path = os.path.join(folder_path, item)
                        if os.path.isfile(item_path):
                            if safe_remove(item_path):
                                removed += 1
                            else:
                                failed.append(item_path)
                        elif os.path.isdir(item_path):
                            if safe_delete_folder(item_path):
                                removed += 1
                            else:
                                failed.append(item_path)

            if failed:
                self.finished_signal.emit(False, t("{} itens removidos, mas {} falharam:\n{}").format(removed, len(failed), "\n".join(failed[:10])))
            else:
                self.finished_signal.emit(True, t("{} item(ns) de cache removido(s) com sucesso!").format(removed))
        except Exception as e:
            self.finished_signal.emit(False, t("Erro ao limpar cache: {}").format(str(e)))

class BackupWorker(QThread):
    finished_signal = pyqtSignal(bool, str)
    def run(self):
        sims_path = ConfigManager.get("sims4_path")
        if not sims_path or not os.path.exists(sims_path):
            self.finished_signal.emit(False, t("Pasta do The Sims 4 não configurada."))
            return
            
        saves_dir = os.path.join(sims_path, "saves")
        if not os.path.exists(saves_dir):
            self.finished_signal.emit(False, t("Pasta 'saves' não encontrada."))
            return
            
        backup_dir = os.path.join(get_documents_dir(), "S4Suite_Backups")
        os.makedirs(backup_dir, exist_ok=True)
        
        date_str = datetime.datetime.now().strftime("%d-%m-%Y_%H-%M-%S")
        backup_filename = os.path.join(backup_dir, f"Saves_BKP_{date_str}")
        
        try:
            with tempfile.TemporaryDirectory() as temp_dir:
                temp_backup_base = os.path.join(temp_dir, f"Saves_BKP_{date_str}")
                temp_zip = shutil.make_archive(temp_backup_base, 'zip', saves_dir)
                final_zip = unique_dest_path(backup_dir, os.path.basename(backup_filename) + ".zip")
                shutil.move(temp_zip, final_zip)
            self.finished_signal.emit(True, t("Backup concluído com sucesso em:\n{}").format(final_zip))
        except Exception as e:
            self.finished_signal.emit(False, t("Erro ao criar backup: {}").format(e))

class StatsWorker(QThread):
    stats_signal = pyqtSignal(dict)

    def run(self):
        stats = {
            "mods_count": 0,
            "scripts_count": 0,
            "tray_count": 0,
            "mods_size_bytes": 0,
            "folder_sizes": {},
            "scripts_list": [],
            "tray_list": []
        }
        
        mods_dir = get_mods_dir()
        if mods_dir and os.path.exists(mods_dir):
            for root, dirs, files in os.walk(mods_dir):
                for f in files:
                    file_path = os.path.join(root, f)
                    
                    if f.lower().endswith('.package'):
                        stats["mods_count"] += 1
                        size = os.path.getsize(file_path)
                        stats["mods_size_bytes"] += size
                        
                        rel_path = os.path.relpath(root, mods_dir)
                        if rel_path == ".":
                            folder_name = t("(Pasta Raiz)")
                        else:
                            folder_name = rel_path.split(os.sep)[0]
                            
                        stats["folder_sizes"][folder_name] = stats["folder_sizes"].get(folder_name, 0) + size
                        
                    elif f.lower().endswith('.ts4script'):
                        stats["scripts_count"] += 1
                        stats["scripts_list"].append(file_path)
                        
                        size = os.path.getsize(file_path)
                        stats["mods_size_bytes"] += size
                        
                        rel_path = os.path.relpath(root, mods_dir)
                        if rel_path == ".":
                            folder_name = t("(Pasta Raiz)")
                        else:
                            folder_name = rel_path.split(os.sep)[0]
                            
                        stats["folder_sizes"][folder_name] = stats["folder_sizes"].get(folder_name, 0) + size

        tray_dir = get_tray_dir()
        if tray_dir and os.path.exists(tray_dir):
            for f in os.listdir(tray_dir):
                file_path = os.path.join(tray_dir, f)
                if os.path.isfile(file_path):
                    stats["tray_list"].append(file_path)
            stats["tray_count"] = len(stats["tray_list"])

        self.stats_signal.emit(stats)

class FilesListDialog(QDialog):
    def __init__(self, file_paths, title_text, parent=None):
        super().__init__(parent)
        self.setWindowTitle(title_text)
        self.resize(600, 400)
        
        layout = QVBoxLayout(self)
        
        title = QLabel(f"{title_text} " + t("(Clique no item para copiar o caminho)"))
        title.setStyleSheet("font-size: 16px; font-weight: bold; ")
        layout.addWidget(title)
        
        self.list_widget = QListWidget()
        
        for path in file_paths:
            safe_path = path.encode('utf-8', 'replace').decode('utf-8')
            item = QListWidgetItem(safe_path)
            self.list_widget.addItem(item)
            
        self.list_widget.itemClicked.connect(self.on_item_clicked)
        layout.addWidget(self.list_widget)
        
        btn_close = QPushButton(t("Fechar"))
        btn_close.clicked.connect(self.accept)
        layout.addWidget(btn_close)
        
    def on_item_clicked(self, item):
        original_text = item.text()
        copied_text = t(" [Copiado!]")
        if original_text.endswith(copied_text):
            original_text = original_text.replace(copied_text, "")
            
        QApplication.clipboard().setText(original_text)
        item.setText(original_text + copied_text)

class FolderDetailsDialog(QDialog):
    def __init__(self, folder_sizes, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Detalhes de Tamanho das Pastas"))
        self.resize(400, 500)
        
        layout = QVBoxLayout(self)
        
        title = QLabel(t("Tamanho Ocupado por Pasta"))
        title.setStyleSheet("font-size: 16px; font-weight: bold; ")
        layout.addWidget(title)
        
        list_widget = QListWidget()
        
        sorted_folders = sorted(folder_sizes.items(), key=lambda x: x[1], reverse=True)
        
        for folder, size in sorted_folders:
            safe_folder = folder.encode('utf-8', 'replace').decode('utf-8')
            item = QListWidgetItem(f"📂 {safe_folder}  →  {format_size(size)}")
            item.setTextAlignment(Qt.AlignmentFlag.AlignVCenter)
            list_widget.addItem(item)
            
        layout.addWidget(list_widget)
        
        btn_close = QPushButton(t("Fechar"))
        btn_close.clicked.connect(self.accept)
        layout.addWidget(btn_close)

class DashboardTab(QWidget):
    def __init__(self):
        super().__init__()
        self.last_stats = None
        self.build_ui()
        self.refresh_stats()

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        layout.setSpacing(20)
        
        # Header
        header_layout = QVBoxLayout()
        header_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        
        title = QLabel(t("🎉 Bem-vindo ao S4 Suite v2.0"))
        title.setStyleSheet("font-size: 26px; font-weight: bold; margin-bottom: 10px; margin-top: 20px;")
        title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        header_layout.addWidget(title)

        subtitle = QLabel(t("O Seu Hub Definitivo para Mods do The Sims 4"))
        subtitle.setStyleSheet("font-size: 16px; ")
        subtitle.setAlignment(Qt.AlignmentFlag.AlignCenter)
        header_layout.addWidget(subtitle)
        
        layout.addLayout(header_layout)

        # Barra de Ações (Atualizar, Cache, Backup)
        action_layout = QHBoxLayout()
        action_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        
        self.btn_refresh = QPushButton(t("🔄 Atualizar Estatísticas"))
        self.btn_refresh.clicked.connect(self.refresh_stats)
        action_layout.addWidget(self.btn_refresh)
        
        self.btn_cache = QPushButton(t("🧹 Limpar Cache do Jogo"))
        self.btn_cache.setObjectName("danger")
        self.btn_cache.clicked.connect(self.clear_cache)
        action_layout.addWidget(self.btn_cache)
        
        self.btn_backup = QPushButton(t("🛡️ Backup de Saves"))
        self.btn_backup.setObjectName("primary")
        self.btn_backup.clicked.connect(self.backup_saves)
        action_layout.addWidget(self.btn_backup)
        
        layout.addLayout(action_layout)

        # Grid de Estatísticas (Cards)
        self.grid_layout = QGridLayout()
        self.grid_layout.setSpacing(15)
        
        self.lbl_mods_count = self.create_stat_card(t("📦 Mods Instalados"), t("Carregando..."), self.grid_layout, 0, 0)
        self.lbl_scripts_count = self.create_clickable_card(t("📜 Scripts Instalados"), t("Carregando..."), t("Ver Scripts"), self.grid_layout, 0, 1, self.show_scripts_details)
        self.lbl_mods_size = self.create_clickable_card(t("💾 Tamanho Total"), t("Carregando..."), t("Ver Pastas"), self.grid_layout, 1, 0, self.show_size_details)
        self.lbl_tray_count = self.create_clickable_card(t("🗂️ Arquivos Tray (Sims/Lotes)"), t("Carregando..."), t("Ver Arquivos"), self.grid_layout, 1, 1, self.show_tray_details)

        layout.addLayout(self.grid_layout)
        layout.addStretch()

    def clear_cache(self):
        self.btn_cache.setEnabled(False)
        self.btn_cache.setText(t("🧹 Limpando..."))
        self.cache_worker = ClearCacheWorker()
        self.cache_worker.finished_signal.connect(self.on_cache_finished)
        self.cache_worker.start()
        
    def on_cache_finished(self, success, msg):
        self.btn_cache.setEnabled(True)
        self.btn_cache.setText(t("🧹 Limpar Cache do Jogo"))
        if success: QMessageBox.information(self, t("Sucesso"), msg)
        else: QMessageBox.warning(self, t("Erro"), msg)
        
    def backup_saves(self):
        self.btn_backup.setEnabled(False)
        self.btn_backup.setText(t("🛡️ Compactando..."))
        self.backup_worker = BackupWorker()
        self.backup_worker.finished_signal.connect(self.on_backup_finished)
        self.backup_worker.start()
        
    def on_backup_finished(self, success, msg):
        self.btn_backup.setEnabled(True)
        self.btn_backup.setText(t("🛡️ Backup de Saves"))
        if success: QMessageBox.information(self, t("Sucesso"), msg)
        else: QMessageBox.warning(self, t("Erro"), msg)

    def create_stat_card(self, title_text, value_text, grid, row, col):
        card = QFrame()
        card.setProperty("class", "StatCard")
        card_layout = QVBoxLayout(card)
        card_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.setContentsMargins(20, 20, 20, 20)
        
        lbl_title = QLabel(title_text)
        lbl_title.setStyleSheet("border: none; opacity: 0.8; font-weight: bold;")
        lbl_title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.addWidget(lbl_title)
        
        lbl_value = QLabel(value_text)
        lbl_value.setStyleSheet("font-size: 26px; font-weight: bold; border: none;")
        lbl_value.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.addWidget(lbl_value)
        
        grid.addWidget(card, row, col)
        return lbl_value

    def create_clickable_card(self, title_text, value_text, hint_text, grid, row, col, click_callback):
        card = QFrame()
        card.setProperty("class", "StatCard")
        
        card_layout = QVBoxLayout(card)
        card_layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.setContentsMargins(20, 20, 20, 20)
        
        lbl_title = QLabel(title_text)
        lbl_title.setStyleSheet("border: none; opacity: 0.8; font-weight: bold;")
        lbl_title.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.addWidget(lbl_title)
        
        lbl_value = QLabel(value_text)
        lbl_value.setStyleSheet("font-size: 26px; font-weight: bold; border: none;")
        lbl_value.setAlignment(Qt.AlignmentFlag.AlignCenter)
        card_layout.addWidget(lbl_value)
        
        btn_details = QPushButton(hint_text)
        btn_details.setObjectName("primary")
        btn_details.setCursor(Qt.CursorShape.PointingHandCursor)
        btn_details.clicked.connect(click_callback)
        card_layout.addWidget(btn_details)
        
        grid.addWidget(card, row, col)
        return lbl_value

    def show_size_details(self):
        if self.last_stats and "folder_sizes" in self.last_stats:
            dialog = FolderDetailsDialog(self.last_stats["folder_sizes"], self)
            dialog.exec()

    def show_scripts_details(self):
        if self.last_stats and self.last_stats.get("scripts_list"):
            dialog = FilesListDialog(self.last_stats["scripts_list"], t("Caminhos dos Scripts"), self)
            dialog.exec()
            
    def show_tray_details(self):
        if self.last_stats and self.last_stats.get("tray_list"):
            dialog = FilesListDialog(self.last_stats["tray_list"], t("Arquivos do Tray Importer"), self)
            dialog.exec()

    def refresh_stats(self):
        self.btn_refresh.setEnabled(False)
        self.btn_refresh.setText(t("⏳ Carregando..."))
        
        self.lbl_mods_count.setText("...")
        self.lbl_scripts_count.setText("...")
        self.lbl_mods_size.setText("...")
        self.lbl_tray_count.setText("...")

        self.worker = StatsWorker()
        self.worker.stats_signal.connect(self.update_stats_ui)
        self.worker.start()

    def update_stats_ui(self, stats):
        self.last_stats = stats
        self.lbl_mods_count.setText(f"{stats['mods_count']}")
        self.lbl_scripts_count.setText(f"{stats['scripts_count']}")
        self.lbl_mods_size.setText(format_size(stats['mods_size_bytes']))
        self.lbl_tray_count.setText(f"{stats['tray_count']}")
        
        self.btn_refresh.setText(t("🔄 Atualizar Estatísticas"))
        self.btn_refresh.setEnabled(True)
