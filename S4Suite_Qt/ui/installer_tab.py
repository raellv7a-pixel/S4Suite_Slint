from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel,
    QPushButton, QFileDialog, QGroupBox, QProgressBar, QTextEdit,
    QMessageBox, QDialog, QListWidget, QListWidgetItem, QRadioButton, QButtonGroup,
    QScrollArea, QTreeWidget, QTreeWidgetItem, QSplitter, QInputDialog, QCheckBox,
    QAbstractItemView, QHeaderView
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal, QMutex, QWaitCondition
from s4common import get_mods_dir
from s4installer import install_mod, careful_scan_mod
import os
from s4translator import t

# ====================================================================
# DIALOGOS INTERATIVOS
# ====================================================================

class ExclusiveSelectionDialog(QDialog):
    def __init__(self, mod_name, options, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Opções Excludentes Detectadas"))
        self.resize(400, 300)
        self.selected_file = None
        
        layout = QVBoxLayout(self)
        
        lbl_info = QLabel(t("O mod <b>{}</b> contém várias versões ou opções que não podem ser instaladas juntas.").format(os.path.basename(mod_name)))
        lbl_info.setWordWrap(True)
        layout.addWidget(lbl_info)
        
        lbl_ask = QLabel(t("Escolha apenas UMA opção para instalar:"))
        lbl_ask.setStyleSheet("font-weight: bold; margin-top: 10px;")
        layout.addWidget(lbl_ask)
        
        self.bg = QButtonGroup(self)
        self.radio_buttons = []
        
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        container = QWidget()
        v_layout = QVBoxLayout(container)
        
        for idx, opt in enumerate(options):
            rb = QRadioButton(opt)
            if idx == 0: rb.setChecked(True)
            self.bg.addButton(rb)
            self.radio_buttons.append((rb, opt))
            v_layout.addWidget(rb)
            
        scroll.setWidget(container)
        layout.addWidget(scroll)
        
        btn_ok = QPushButton(t("Confirmar Escolha"))
        btn_ok.setObjectName("primary")
        btn_ok.clicked.connect(self.on_confirm)
        layout.addWidget(btn_ok)
        
    def on_confirm(self):
        for rb, opt in self.radio_buttons:
            if rb.isChecked():
                self.selected_file = opt
                break
        self.accept()

class DecisionDialog(QDialog):
    def __init__(self, mod_name, existing_path, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Mod Já Existe"))
        self.resize(400, 200)
        self.decision = False
        
        layout = QVBoxLayout(self)
        
        lbl_info = QLabel(t("O mod <b>{}</b> já existe na pasta:<br><i>{}</i>").format(os.path.basename(mod_name), existing_path))
        lbl_info.setWordWrap(True)
        layout.addWidget(lbl_info)
        
        lbl_ask = QLabel(t("Deseja substituí-lo (Atualizar)?"))
        lbl_ask.setStyleSheet("font-weight: bold; margin-top: 10px;")
        layout.addWidget(lbl_ask)
        
        row = QHBoxLayout()
        btn_yes = QPushButton(t("Sim, Atualizar"))
        btn_yes.setObjectName("primary")
        btn_yes.clicked.connect(self.on_yes)
        
        btn_no = QPushButton(t("Não, Pular"))
        btn_no.clicked.connect(self.on_no)
        
        row.addWidget(btn_yes)
        row.addWidget(btn_no)
        layout.addLayout(row)
        
    def on_yes(self):
        self.decision = True
        self.accept()
        
    def on_no(self):
        self.decision = False
        self.accept()

class CarefulScanDialog(QDialog):
    def __init__(self, mod_name, report, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Resultado do Scan Cuidadoso"))
        self.resize(600, 400)
        self.should_install = False
        
        layout = QVBoxLayout(self)
        
        lbl_info = QLabel(t("Resultados para: <b>{}</b>").format(os.path.basename(mod_name)))
        lbl_info.setStyleSheet("font-size: 16px;")
        layout.addWidget(lbl_info)
        
        list_widget = QListWidget()
        
        novos = 0
        conflitos = 0
        duplicatas = 0
        
        for item in report:
            status = item['status']
            fname = item['file']
            
            if status == 'NEW':
                text = t("🟩 NOVO: {}").format(fname)
                novos += 1
            elif status == 'UPDATE_DETECTED':
                text = t("🟨 POSSÍVEL ATUALIZAÇÃO: {}").format(fname)
                conflitos += 1
            elif status == 'SIZE_DIFF':
                text = t("🟨 CONFLITO DE NOME: {} (mesmo arquivo, tamanho diferente)").format(fname)
                conflitos += 1
            elif status == 'EXACT_MATCH':
                text = t("⬛ DUPLICATA IGNORADA: {} (Você já tem exatamente este arquivo)").format(fname)
                duplicatas += 1
            else:
                text = f"⬛ {fname}"
                
            li = QListWidgetItem(text)
            list_widget.addItem(li)
            
        layout.addWidget(list_widget)
        
        resumo = QLabel(t("<b>Resumo:</b> {} Novos, {} Conflitos, {} Duplicatas Exatas.").format(novos, conflitos, duplicatas))
        layout.addWidget(resumo)
        
        row = QHBoxLayout()
        btn_install = QPushButton(t("Instalar (Ignorar Duplicatas)"))
        btn_install.setObjectName("primary")
        btn_install.clicked.connect(self.on_install)
        
        btn_cancel = QPushButton(t("Cancelar Instalação"))
        btn_cancel.clicked.connect(self.reject)
        
        row.addWidget(btn_install)
        row.addWidget(btn_cancel)
        layout.addLayout(row)
        
    def on_install(self):
        self.should_install = True
        self.accept()

# ====================================================================
# WORKER THREAD
# ====================================================================
class InstallerWorker(QThread):
    progress_signal = pyqtSignal(int)
    log_signal = pyqtSignal(str)
    finished_signal = pyqtSignal(bool)

    ask_selection_signal = pyqtSignal(str, list)
    ask_decision_signal = pyqtSignal(str, str)
    scan_report_signal = pyqtSignal(str, list)

    def __init__(self, install_jobs, mods_dir, is_careful=False):
        super().__init__()
        self.install_jobs = install_jobs
        self.mods_dir = mods_dir
        self.is_careful = is_careful

        self.wait_mutex = QMutex()
        self.wait_condition = QWaitCondition()
        self.response_ready = False
        self.user_selection = None
        self.user_decision = None
        self.user_wants_careful_install = False

    def prepare_ui_wait(self):
        self.wait_mutex.lock()
        self.response_ready = False
        self.wait_mutex.unlock()

    def wait_for_ui(self):
        self.wait_mutex.lock()
        while not self.response_ready:
            if not self.wait_condition.wait(self.wait_mutex, 300000):
                self.response_ready = True
                self.user_selection = None
                self.user_decision = False
                self.user_wants_careful_install = False
                self.log_signal.emit(t("⚠️ Tempo de resposta esgotado. Operação atual cancelada."))
                break
        self.wait_mutex.unlock()

    def wake_from_ui(self):
        self.wait_mutex.lock()
        self.response_ready = True
        self.wait_condition.wakeAll()
        self.wait_mutex.unlock()

    def run(self):
        try:
            total = sum(len(job["files"]) for job in self.install_jobs)
            processed = 0
            success_count = 0

            for job in self.install_jobs:
                target_dir = job["target_dir"]
                files = job["files"]
                self.log_signal.emit(t("📁 Destino: {}").format(os.path.basename(target_dir)))

                for file_path in files:
                    self.user_selection = None
                    self.user_decision = None
                    self.user_wants_careful_install = False

                    mod_basename = os.path.basename(file_path)
                    self.log_signal.emit(t("Processando: {}").format(mod_basename))

                    if self.is_careful:
                        report, err = careful_scan_mod(file_path, self.mods_dir)
                        if err:
                            self.log_signal.emit(t("❌ Erro no scan: {}").format(err))
                            processed += 1
                            self.progress_signal.emit(int((processed / total) * 100))
                            continue

                        self.prepare_ui_wait()
                        self.scan_report_signal.emit(file_path, report)
                        self.wait_for_ui()

                        if not self.user_wants_careful_install:
                            self.log_signal.emit(t("⚠️ Instalação cancelada após scan."))
                            processed += 1
                            self.progress_signal.emit(int((processed / total) * 100))
                            continue

                    status, msg = install_mod(file_path, self.mods_dir, target_base_dir=target_dir)

                    if status == "NEEDS_SELECTION":
                        self.prepare_ui_wait()
                        self.ask_selection_signal.emit(file_path, msg)
                        self.wait_for_ui()
                        if not self.user_selection:
                            self.log_signal.emit(t("❌ Instalação pulada (Seleção Excludente cancelada)."))
                            processed += 1
                            self.progress_signal.emit(int((processed / total) * 100))
                            continue
                        status, msg = install_mod(
                            file_path,
                            self.mods_dir,
                            selected_file=self.user_selection,
                            target_base_dir=target_dir,
                        )

                    if status == "NEEDS_DECISION":
                        self.prepare_ui_wait()
                        self.ask_decision_signal.emit(file_path, msg)
                        self.wait_for_ui()
                        if self.user_decision is False:
                            self.log_signal.emit(t("❌ Instalação pulada pelo usuário."))
                            processed += 1
                            self.progress_signal.emit(int((processed / total) * 100))
                            continue
                        status, msg = install_mod(
                            file_path,
                            self.mods_dir,
                            force_update=self.user_decision,
                            target_base_dir=target_dir,
                        )

                    if status is True:
                        self.log_signal.emit(t("✅ Instalado em: {}\n").format(msg))
                        success_count += 1
                    elif status is False:
                        self.log_signal.emit(t("❌ Falha ou Cancelado: {}\n").format(msg))

                    processed += 1
                    self.progress_signal.emit(int((processed / total) * 100))

            self.log_signal.emit(t("Operação Concluída. {} de {} processados com sucesso.").format(success_count, total))
            self.finished_signal.emit(success_count == total)
        except Exception as e:
            self.log_signal.emit(t("❌ Erro inesperado no instalador: {}").format(e))
            self.finished_signal.emit(False)

# ====================================================================
# INTERFACE GRÁFICA (UI) DA ABA DE INSTALAÇÃO
# ====================================================================
class InstallerTab(QWidget):
    def __init__(self):
        super().__init__()
        self.install_queue = {}
        self.selected_target_dir = None
        self.folder_items = {}
        self.setAcceptDrops(True)
        self.build_ui()
        self.load_folders()

    def dragEnterEvent(self, event):
        if event.mimeData().hasUrls():
            event.accept()
        else:
            event.ignore()

    def dropEvent(self, event):
        files = [u.toLocalFile() for u in event.mimeData().urls()]
        self.add_files_to_selected_folder(files)

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(12)

        title = QLabel(t("📦 Instalar e Organizar Mods"))
        title.setObjectName("HeaderTitle")
        layout.addWidget(title)

        desc = QLabel(t("Escolha uma pasta de destino, adicione os mods dessa pasta e monte uma fila organizada antes de instalar."))
        desc.setObjectName("SubDescription")
        desc.setWordWrap(True)
        layout.addWidget(desc)

        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter, stretch=3)

        folders_group = QGroupBox(t("Pastas em Mods"))
        folders_layout = QVBoxLayout(folders_group)

        folder_actions = QHBoxLayout()
        btn_refresh = QPushButton(t("🔄 Recarregar"))
        btn_refresh.clicked.connect(self.load_folders)
        folder_actions.addWidget(btn_refresh)

        btn_create = QPushButton(t("➕ Nova Pasta"))
        btn_create.clicked.connect(self.on_create_folder)
        folder_actions.addWidget(btn_create)
        folders_layout.addLayout(folder_actions)

        self.folder_tree = QTreeWidget()
        self.folder_tree.setColumnCount(1)
        self.folder_tree.setHeaderHidden(True)
        self.folder_tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.folder_tree.setTextElideMode(Qt.TextElideMode.ElideNone)
        self.folder_tree.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.folder_tree.setIndentation(18)
        self.folder_tree.setUniformRowHeights(True)
        self.folder_tree.setStyleSheet("""
            QTreeWidget { padding: 8px 10px; }
            QTreeWidget::item { min-height: 34px; padding: 4px 8px; }
        """)
        self.folder_tree.itemSelectionChanged.connect(self.on_folder_selected)
        folders_layout.addWidget(self.folder_tree)
        splitter.addWidget(folders_group)

        selection_group = QGroupBox(t("Arquivos para a Pasta Selecionada"))
        selection_layout = QVBoxLayout(selection_group)

        self.lbl_selected_folder = QLabel(t("Selecione uma pasta em Mods."))
        self.lbl_selected_folder.setWordWrap(True)
        selection_layout.addWidget(self.lbl_selected_folder)

        self.current_folder_files = QListWidget()
        self.current_folder_files.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        selection_layout.addWidget(self.current_folder_files)

        selected_actions = QHBoxLayout()
        btn_add = QPushButton(t("📁 Adicionar Mods"))
        btn_add.setObjectName("primary")
        btn_add.clicked.connect(self.on_browse)
        selected_actions.addWidget(btn_add)

        btn_remove_current = QPushButton(t("Remover da Pasta"))
        btn_remove_current.clicked.connect(self.remove_selected_from_current_folder)
        selected_actions.addWidget(btn_remove_current)
        selection_layout.addLayout(selected_actions)
        splitter.addWidget(selection_group)

        queue_group = QGroupBox(t("Fila de Instalação"))
        queue_layout = QVBoxLayout(queue_group)

        self.lbl_queue_summary = QLabel(t("Nenhum mod aguardando instalação."))
        self.lbl_queue_summary.setWordWrap(True)
        queue_layout.addWidget(self.lbl_queue_summary)

        self.queue_list = QListWidget()
        self.queue_list.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        queue_layout.addWidget(self.queue_list)

        queue_actions = QHBoxLayout()
        btn_remove = QPushButton(t("Remover Selecionados"))
        btn_remove.clicked.connect(self.remove_selected_from_queue)
        queue_actions.addWidget(btn_remove)

        btn_clear = QPushButton(t("Limpar Fila"))
        btn_clear.clicked.connect(self.clear_queue)
        queue_actions.addWidget(btn_clear)
        queue_layout.addLayout(queue_actions)

        self.chk_careful = QCheckBox(t("Executar scan cuidadoso antes de instalar"))
        queue_layout.addWidget(self.chk_careful)

        self.btn_install = QPushButton(t("Instalar Mods"))
        self.btn_install.setObjectName("primary")
        self.btn_install.clicked.connect(self.start_installation)
        queue_layout.addWidget(self.btn_install)
        splitter.addWidget(queue_group)

        splitter.setSizes([260, 360, 320])

        self.progress_bar = QProgressBar()
        self.progress_bar.setValue(0)
        self.progress_bar.setTextVisible(True)
        layout.addWidget(self.progress_bar)

        self.log_console = QTextEdit()
        self.log_console.setReadOnly(True)
        layout.addWidget(self.log_console, stretch=2)

    def get_valid_mod_files(self, files):
        return [f for f in files if f.lower().endswith(('.zip', '.7z', '.rar', '.package', '.ts4script'))]

    def load_folders(self):
        mods_dir = get_mods_dir()
        self.folder_tree.clear()
        self.folder_items = {}

        if not mods_dir or not os.path.isdir(mods_dir):
            self.append_log(t("❌ Caminho de Mods não configurado. Vá na aba Configurações."))
            return

        def add_children(parent_item, path):
            try:
                entries = sorted((e for e in os.scandir(path) if e.is_dir()), key=lambda e: e.name.lower())
            except Exception as e:
                self.append_log(t("⚠️ Falha ao ler pasta {}: {}").format(path, e))
                return
            for entry in entries:
                item = QTreeWidgetItem([entry.name])
                item.setToolTip(0, entry.name)
                item.setData(0, Qt.ItemDataRole.UserRole, entry.path)
                parent_item.addChild(item)
                self.folder_items[entry.path] = item
                add_children(item, entry.path)

        root_item = QTreeWidgetItem([t("Mods")])
        root_item.setData(0, Qt.ItemDataRole.UserRole, mods_dir)
        root_item.setFlags(root_item.flags() & ~Qt.ItemFlag.ItemIsSelectable)
        self.folder_tree.addTopLevelItem(root_item)
        add_children(root_item, mods_dir)
        root_item.setExpanded(True)
        self.refresh_queue_views()

    def on_folder_selected(self):
        items = self.folder_tree.selectedItems()
        self.selected_target_dir = items[0].data(0, Qt.ItemDataRole.UserRole) if items else None
        if self.selected_target_dir:
            rel = os.path.relpath(self.selected_target_dir, get_mods_dir())
            self.lbl_selected_folder.setText(t("Destino selecionado: Mods/{}").format(rel))
        else:
            self.lbl_selected_folder.setText(t("Selecione uma pasta em Mods."))
        self.refresh_current_folder_files()

    def on_create_folder(self):
        mods_dir = get_mods_dir()
        if not mods_dir:
            self.append_log(t("❌ Caminho de Mods não configurado."))
            return

        parent_dir = self.selected_target_dir or mods_dir
        name, ok = QInputDialog.getText(self, t("Nova Pasta"), t("Nome da nova pasta:"))
        if not ok or not name.strip():
            return

        safe_name = "".join(c for c in name.strip() if c not in '/\\:*?"<>|').strip()
        if not safe_name:
            QMessageBox.warning(self, t("Nome inválido"), t("Escolha um nome de pasta válido."))
            return

        new_dir = os.path.join(parent_dir, safe_name)
        try:
            os.makedirs(new_dir, exist_ok=False)
            self.append_log(t("📁 Pasta criada: {}").format(new_dir))
            self.load_folders()
        except FileExistsError:
            QMessageBox.warning(self, t("Pasta já existe"), t("Já existe uma pasta com esse nome."))
        except Exception as e:
            QMessageBox.critical(self, t("Erro"), t("Falha ao criar pasta: {}").format(e))

    def on_browse(self):
        if not self.selected_target_dir:
            self.append_log(t("⚠️ Selecione uma pasta de destino antes de adicionar mods."))
            return

        downloads_dir = os.path.expanduser("~/Downloads")
        files, _ = QFileDialog.getOpenFileNames(
            self,
            t("Selecione os Mods"),
            downloads_dir,
            t("Arquivos Suportados (*.zip *.7z *.rar *.package *.ts4script);;Todos (*.*)"),
            options=QFileDialog.Option.DontUseNativeDialog
        )
        self.add_files_to_selected_folder(files)

    def add_files_to_selected_folder(self, files):
        if not self.selected_target_dir:
            self.append_log(t("⚠️ Selecione uma pasta de destino antes de adicionar mods."))
            return

        valid_files = self.get_valid_mod_files(files)
        if not valid_files:
            self.append_log(t("⚠️ Nenhum arquivo de mod válido foi selecionado."))
            return

        queued = self.install_queue.setdefault(self.selected_target_dir, [])
        added = 0
        for file_path in valid_files:
            if file_path not in queued:
                queued.append(file_path)
                added += 1

        rel = os.path.relpath(self.selected_target_dir, get_mods_dir())
        self.append_log(t("➕ {} arquivo(s) adicionados para Mods/{}.").format(added, rel))
        self.refresh_queue_views()

    def refresh_queue_views(self):
        self.refresh_current_folder_files()
        self.queue_list.clear()

        total_files = 0
        active_targets = 0
        mods_dir = get_mods_dir()
        for target_dir in sorted(self.install_queue):
            files = self.install_queue[target_dir]
            if not files:
                continue
            active_targets += 1
            total_files += len(files)
            rel = os.path.relpath(target_dir, mods_dir) if mods_dir else target_dir
            header = QListWidgetItem(t("📁 Mods/{} ({} arquivo(s))").format(rel, len(files)))
            header.setFlags(header.flags() & ~Qt.ItemFlag.ItemIsSelectable)
            self.queue_list.addItem(header)
            for file_path in files:
                item = QListWidgetItem("  " + os.path.basename(file_path))
                item.setData(Qt.ItemDataRole.UserRole, (target_dir, file_path))
                self.queue_list.addItem(item)

        for path, item in self.folder_items.items():
            count = len(self.install_queue.get(path, []))
            folder_name = os.path.basename(path)
            if count:
                item.setText(0, t("{}  ·  {} aguardando").format(folder_name, count))
            else:
                item.setText(0, folder_name)

        if total_files:
            self.lbl_queue_summary.setText(t("{} mod(s) aguardando instalação em {} pasta(s).").format(total_files, active_targets))
        else:
            self.lbl_queue_summary.setText(t("Nenhum mod aguardando instalação."))

    def refresh_current_folder_files(self):
        if not hasattr(self, "current_folder_files"):
            return
        self.current_folder_files.clear()
        if not self.selected_target_dir:
            return
        for file_path in self.install_queue.get(self.selected_target_dir, []):
            item = QListWidgetItem(os.path.basename(file_path))
            item.setData(Qt.ItemDataRole.UserRole, file_path)
            self.current_folder_files.addItem(item)

    def remove_selected_from_current_folder(self):
        if not self.selected_target_dir:
            return
        selected = self.current_folder_files.selectedItems()
        if not selected:
            return
        queued = self.install_queue.get(self.selected_target_dir, [])
        for item in selected:
            file_path = item.data(Qt.ItemDataRole.UserRole)
            if file_path in queued:
                queued.remove(file_path)
        if not queued:
            self.install_queue.pop(self.selected_target_dir, None)
        self.refresh_queue_views()

    def remove_selected_from_queue(self):
        selected = self.queue_list.selectedItems()
        if not selected:
            return
        for item in selected:
            data = item.data(Qt.ItemDataRole.UserRole)
            if not data:
                continue
            target_dir, file_path = data
            queued = self.install_queue.get(target_dir, [])
            if file_path in queued:
                queued.remove(file_path)
            if not queued:
                self.install_queue.pop(target_dir, None)
        self.refresh_queue_views()

    def clear_queue(self):
        self.install_queue.clear()
        self.refresh_queue_views()

    def append_log(self, text):
        self.log_console.append(text)
        scrollbar = self.log_console.verticalScrollBar()
        scrollbar.setValue(scrollbar.maximum())

    def build_install_jobs(self):
        return [
            {"target_dir": target_dir, "files": list(files)}
            for target_dir, files in self.install_queue.items()
            if files
        ]

    def start_installation(self):
        jobs = self.build_install_jobs()
        if not jobs:
            self.append_log(t("⚠️ Adicione mods à fila antes de instalar."))
            return

        mods_dir = get_mods_dir()
        if not mods_dir:
            self.append_log(t("❌ ERRO: Caminho do The Sims 4 não configurado. Vá na aba Configurações."))
            return

        self.btn_install.setEnabled(False)
        self.progress_bar.setValue(0)
        self.log_console.clear()

        total = sum(len(job["files"]) for job in jobs)
        self.append_log(t("🚀 Iniciando instalação organizada de {} mod(s) em {} pasta(s)...").format(total, len(jobs)))

        self.worker = InstallerWorker(jobs, mods_dir, is_careful=self.chk_careful.isChecked())
        self.worker.progress_signal.connect(self.progress_bar.setValue)
        self.worker.log_signal.connect(self.append_log)
        self.worker.finished_signal.connect(self.on_installation_finished)

        self.worker.ask_selection_signal.connect(self.handle_ask_selection)
        self.worker.ask_decision_signal.connect(self.handle_ask_decision)
        self.worker.scan_report_signal.connect(self.handle_scan_report)

        self.worker.start()

    def handle_ask_selection(self, mod_name, options):
        dialog = ExclusiveSelectionDialog(mod_name, options, self)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            self.worker.user_selection = dialog.selected_file
        else:
            self.worker.user_selection = None
        self.worker.wake_from_ui()

    def handle_ask_decision(self, mod_name, existing_path):
        dialog = DecisionDialog(mod_name, existing_path, self)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            self.worker.user_decision = dialog.decision
        else:
            self.worker.user_decision = False
        self.worker.wake_from_ui()

    def handle_scan_report(self, mod_name, report):
        dialog = CarefulScanDialog(mod_name, report, self)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            self.worker.user_wants_careful_install = dialog.should_install
        else:
            self.worker.user_wants_careful_install = False
        self.worker.wake_from_ui()

    def on_installation_finished(self, success):
        self.btn_install.setEnabled(True)
        if success:
            self.clear_queue()
            self.load_folders()
        else:
            self.append_log(t("⚠️ A fila foi mantida porque nem todos os mods foram instalados. Revise o log antes de tentar novamente."))
