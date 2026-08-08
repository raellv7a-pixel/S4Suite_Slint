from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QPushButton, QFileDialog,
    QGroupBox, QProgressBar, QTextEdit, QMessageBox, QInputDialog,
    QTreeWidget, QTreeWidgetItem, QComboBox, QSplitter, QAbstractItemView,
    QHeaderView, QListWidget, QListWidgetItem,
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal

from s4common import ConfigManager, get_mods_dir, is_path_inside, safe_remove, unique_dest_path
from s4disabled import disable_mod
from s4merger import calculate_merge_stats, merge_sims4_packages, prepare_temp_merge_area
from s4translator import t

import datetime
import os
import shutil
import tempfile


POST_MODES = {
    "keep": "Gerar merge e manter originais",
    "disable": "Gerar merge e desativar originais",
    "backup": "Gerar merge e mover originais para backup",
    "delete": "Gerar merge e excluir originais",
}

RISK_KEYWORDS = [
    "script", "scripts", "ts4script", "ui", "tuning", "xml", "injector",
    "mccc", "mc_cmd", "wicked", "basemental", "tmex", "betterexceptions",
    "default", "replacement", "override", "core", "trait", "traits",
    "gameplay", "fix", "bug",
]


def format_size(num_bytes):
    value = float(num_bytes or 0)
    for unit in ("B", "KB", "MB", "GB"):
        if value < 1024 or unit == "GB":
            return f"{value:.1f} {unit}" if unit != "B" else f"{int(value)} B"
        value /= 1024
    return f"{value:.1f} GB"


def list_package_files(folder):
    packages = []
    for root, _, files in os.walk(folder):
        for file in files:
            if file.lower().endswith(".package") and not file.startswith("Merged_"):
                packages.append(os.path.join(root, file))
    packages.sort()
    return packages


def analyze_folder_risks(folder):
    risks = []
    files_seen = []
    for root, _, files in os.walk(folder):
        for file in files:
            path = os.path.join(root, file)
            rel = os.path.relpath(path, folder)
            low = rel.lower()
            files_seen.append(path)
            if low.endswith(".ts4script"):
                risks.append(t("Contém .ts4script. Scripts não entram no merge e costumam acompanhar packages que não devem ser unidos."))
            elif low.endswith(".package") and any(keyword in low for keyword in RISK_KEYWORDS):
                risks.append(t("Nome/pasta sugere mod sensível: {}").format(rel))
    return sorted(set(risks)), files_seen


def is_broad_path(path):
    broad = {
        os.path.realpath("/"),
        os.path.realpath(os.path.expanduser("~")),
        os.path.realpath("/home"),
        os.path.realpath("/tmp"),
    }
    return os.path.realpath(path) in broad


class MergeQueueWorker(QThread):
    progress_signal = pyqtSignal(int, int, str)
    log_signal = pyqtSignal(str)
    job_status_signal = pyqtSignal(int, str)
    finished_signal = pyqtSignal(list)

    def __init__(self, jobs):
        super().__init__()
        self.jobs = jobs

    def run(self):
        results = []
        total_jobs = len(self.jobs)
        for index, job in enumerate(self.jobs):
            self.job_status_signal.emit(index, "running")
            self.log_signal.emit(t("\n▶️ Tarefa {}/{}: {}").format(index + 1, total_jobs, job["name"]))
            temp_dir = None
            input_dir = job["input_dir"]

            try:
                if job.get("source_type") == "files":
                    temp_dir = tempfile.mkdtemp(prefix="s4suite_merge_files_")
                    self.log_signal.emit(t("📦 Preparando arquivos selecionados..."))
                    extracted = prepare_temp_merge_area(job["files"], temp_dir)
                    if not extracted:
                        raise RuntimeError(t("Nenhum arquivo .package encontrado para unificar."))
                    input_dir = temp_dir

                os.makedirs(job["output_dir"], exist_ok=True)
                result = merge_sims4_packages(
                    input_dir=input_dir,
                    output_dir=job["output_dir"],
                    max_size_gb=job["max_size_gb"],
                    log_callback=self.log_signal.emit,
                    progress_callback=lambda current, total, text, i=index: self.progress_signal.emit(current, total, text),
                )

                success = isinstance(result, dict) and result.get("success") is True
                partial = isinstance(result, dict) and result.get("partial") is True
                if success:
                    post = self.apply_post_action(job)
                    status = "done" if not post["failed"] else "warning"
                    self.job_status_signal.emit(index, status)
                    results.append({"job": job, "result": result, "post": post, "status": status})
                elif partial:
                    self.job_status_signal.emit(index, "partial")
                    self.log_signal.emit(t("⚠️ Pós-ação não executada porque o merge foi parcial."))
                    results.append({"job": job, "result": result, "post": None, "status": "partial"})
                else:
                    self.job_status_signal.emit(index, "failed")
                    self.log_signal.emit(t("❌ Pós-ação não executada porque o merge falhou."))
                    results.append({"job": job, "result": result, "post": None, "status": "failed"})
            except Exception as e:
                self.job_status_signal.emit(index, "failed")
                self.log_signal.emit(t("❌ Falha na tarefa '{}': {}").format(job["name"], e))
                results.append({"job": job, "result": {"success": False, "error": str(e)}, "post": None, "status": "failed"})
            finally:
                if temp_dir and os.path.exists(temp_dir):
                    shutil.rmtree(temp_dir, ignore_errors=True)

        self.finished_signal.emit(results)

    def apply_post_action(self, job):
        mode = job.get("post_mode", "keep")
        post = {"mode": mode, "processed": [], "failed": []}
        if mode == "keep" or job.get("source_type") != "folder":
            return post

        packages = list_package_files(job["input_dir"])
        if not packages:
            return post

        archive_dir = None
        if mode == "backup":
            mods_dir = get_mods_dir()
            archive_dir = os.path.join(mods_dir, "_S4Suite_Disabled", "After Merge", job["name"])
            os.makedirs(archive_dir, exist_ok=True)

        for package in packages:
            try:
                if mode == "disable":
                    disabled_path = disable_mod(
                        package,
                        reason="merge_original",
                        source_process="merger",
                        related_output=job["output_dir"],
                    )
                    post["processed"].append(disabled_path)
                elif mode == "backup":
                    disabled_path = disable_mod(
                        package,
                        reason="merge_original",
                        source_process="merger",
                        related_output=job["output_dir"],
                        archive_dir=archive_dir,
                    )
                    post["processed"].append(disabled_path)
                elif mode == "delete":
                    if safe_remove(package):
                        post["processed"].append(package)
                    else:
                        post["failed"].append(package)
            except Exception as e:
                post["failed"].append(f"{package}: {e}")
        return post


class MergerTab(QWidget):
    def __init__(self):
        super().__init__()
        self.jobs = []
        self.build_ui()

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(12)

        title = QLabel(t("🔗 Gerenciador de Merges"))
        title.setObjectName("HeaderTitle")
        layout.addWidget(title)

        desc = QLabel(t("Monte uma fila de merges, revise riscos e escolha o que fazer com os originais após cada merge."))
        desc.setObjectName("SubDescription")
        desc.setWordWrap(True)
        layout.addWidget(desc)

        warning = QLabel(t(
            "<b>Quando evitar merge:</b> scripts, UI mods, gameplay/tuning/XML, core mods, "
            "default replacements/overrides, mods recém-atualizados ou qualquer pacote que você não identifique. "
            "Mais seguro para CAS, cabelos, roupas, acessórios e objetos CC estáticos já testados."
        ))
        warning.setWordWrap(True)
        warning.setStyleSheet("color: #f5c542; padding: 6px 0;")
        layout.addWidget(warning)

        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter, stretch=3)

        left_group = QGroupBox(t("Tarefas de Merge"))
        left_layout = QVBoxLayout(left_group)

        action_row = QHBoxLayout()
        btn_add_folder = QPushButton(t("Adicionar Pasta"))
        btn_add_folder.setObjectName("primary")
        btn_add_folder.clicked.connect(self.on_add_folder)
        action_row.addWidget(btn_add_folder)

        btn_add_files = QPushButton(t("Adicionar Arquivos"))
        btn_add_files.clicked.connect(self.on_add_files)
        action_row.addWidget(btn_add_files)
        left_layout.addLayout(action_row)

        self.jobs_tree = QTreeWidget()
        self.jobs_tree.setObjectName("MergeJobsTree")
        self.jobs_tree.setHeaderLabels([t("Tarefa"), t("Packages"), t("Partes"), t("Modo"), t("Status")])
        self.jobs_tree.header().setMinimumHeight(36)
        header = self.jobs_tree.header()
        header.setDefaultAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        header.setStretchLastSection(False)
        header.setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col, width in ((1, 104), (2, 76), (3, 270), (4, 124)):
            header.setSectionResizeMode(col, QHeaderView.ResizeMode.Interactive)
            header.resizeSection(col, width)
        for col, alignment in enumerate(self.merge_column_alignments()):
            self.jobs_tree.headerItem().setTextAlignment(col, alignment)
        self.jobs_tree.setRootIsDecorated(False)
        self.jobs_tree.setIndentation(0)
        self.jobs_tree.setTextElideMode(Qt.TextElideMode.ElideNone)
        self.jobs_tree.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.jobs_tree.setUniformRowHeights(True)
        self.jobs_tree.setAllColumnsShowFocus(True)
        self.jobs_tree.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.jobs_tree.itemSelectionChanged.connect(self.refresh_details)
        left_layout.addWidget(self.jobs_tree)

        row_manage = QHBoxLayout()
        btn_remove = QPushButton(t("Remover da Fila"))
        btn_remove.clicked.connect(self.on_remove_job)
        row_manage.addWidget(btn_remove)

        btn_dest = QPushButton(t("Editar Destino"))
        btn_dest.clicked.connect(self.on_edit_destination)
        row_manage.addWidget(btn_dest)
        left_layout.addLayout(row_manage)
        splitter.addWidget(left_group)

        detail_group = QGroupBox(t("Detalhes da Tarefa"))
        detail_layout = QVBoxLayout(detail_group)

        self.detail_label = QLabel(t("Nenhuma tarefa selecionada."))
        self.detail_label.setWordWrap(True)
        detail_layout.addWidget(self.detail_label)

        mode_row = QHBoxLayout()
        mode_row.addWidget(QLabel(t("Pós-merge:")))
        self.post_mode_combo = QComboBox()
        for key, label in POST_MODES.items():
            self.post_mode_combo.addItem(t(label), key)
        self.post_mode_combo.currentIndexChanged.connect(self.on_mode_changed)
        mode_row.addWidget(self.post_mode_combo)
        detail_layout.addLayout(mode_row)

        limit_row = QHBoxLayout()
        limit_row.addWidget(QLabel(t("Limite por parte:")))
        self.limit_combo = QComboBox()
        self.limit_combo.addItems(["1.0 GB", "1.5 GB", "1.85 GB", "2.0 GB"])
        self.limit_combo.currentIndexChanged.connect(self.on_limit_changed)
        limit_row.addWidget(self.limit_combo)
        detail_layout.addLayout(limit_row)

        self.risk_list = QListWidget()
        self.risk_list.setObjectName("MergeRiskList")
        self.risk_list.setWordWrap(True)
        self.risk_list.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        detail_layout.addWidget(self.risk_list)

        self.btn_start = QPushButton(t("Iniciar Merges"))
        self.btn_start.setObjectName("primary")
        self.btn_start.clicked.connect(self.start_queue)
        detail_layout.addWidget(self.btn_start)
        splitter.addWidget(detail_group)
        splitter.setSizes([760, 520])

        self.lbl_status = QLabel(t("Aguardando tarefas..."))
        layout.addWidget(self.lbl_status)

        self.progress_bar = QProgressBar()
        self.progress_bar.setValue(0)
        layout.addWidget(self.progress_bar)

        self.log_console = QTextEdit()
        self.log_console.setReadOnly(True)
        layout.addWidget(self.log_console, stretch=2)

    def merge_column_alignments(self):
        left = Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter
        right = Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter
        return [left, right, right, left, left]

    def append_log(self, text):
        self.log_console.append(text)
        scrollbar = self.log_console.verticalScrollBar()
        scrollbar.setValue(scrollbar.maximum())

    def default_limit(self):
        saved = ConfigManager.get("merge_limit", "ask")
        return 1.0 if saved == "ask" else float(saved)

    def default_output_dir(self, name):
        mods_dir = get_mods_dir()
        if not mods_dir:
            return ""
        safe_name = "".join(c if c.isalnum() or c in " _.-" else "_" for c in name).strip() or "Merge"
        return unique_dest_path(os.path.join(mods_dir, "S4Suite_Merged"), safe_name)

    def add_job(self, job):
        self.jobs.append(job)
        self.refresh_jobs_tree()

    def build_folder_job(self, folder):
        limit = self.default_limit()
        count, parts = calculate_merge_stats(folder, limit)
        size = sum(os.path.getsize(path) for path in list_package_files(folder))
        risks, _ = analyze_folder_risks(folder)
        name = os.path.basename(folder) or "Merge"
        return {
            "name": name,
            "source_type": "folder",
            "input_dir": folder,
            "files": [],
            "output_dir": self.default_output_dir(name),
            "max_size_gb": limit,
            "post_mode": "keep",
            "package_count": count,
            "estimated_parts": parts,
            "size": size,
            "risks": risks,
            "status": "waiting",
        }

    def build_files_job(self, files):
        limit = self.default_limit()
        name = datetime.datetime.now().strftime("Novos Downloads %Y-%m-%d %H-%M-%S")
        risks = []
        for path in files:
            low = os.path.basename(path).lower()
            if any(keyword in low for keyword in RISK_KEYWORDS):
                risks.append(t("Nome de arquivo sugere mod sensível: {}").format(os.path.basename(path)))
        return {
            "name": name,
            "source_type": "files",
            "input_dir": "",
            "files": files,
            "output_dir": self.default_output_dir(name),
            "max_size_gb": limit,
            "post_mode": "keep",
            "package_count": len([f for f in files if f.lower().endswith(".package")]),
            "estimated_parts": 1,
            "size": sum(os.path.getsize(f) for f in files if os.path.isfile(f)),
            "risks": sorted(set(risks)),
            "status": "waiting",
        }

    def on_add_folder(self):
        start = get_mods_dir() or os.path.expanduser("~")
        folder = QFileDialog.getExistingDirectory(self, t("Adicionar Pasta ao Merge"), start, options=QFileDialog.Option.DontUseNativeDialog)
        if not folder:
            return
        count, _ = calculate_merge_stats(folder, self.default_limit())
        if count == 0:
            QMessageBox.warning(self, t("Aviso"), t("A pasta selecionada não contém arquivos .package."))
            return
        self.add_job(self.build_folder_job(folder))

    def on_add_files(self):
        start = os.path.expanduser("~/Downloads")
        files, _ = QFileDialog.getOpenFileNames(
            self,
            t("Adicionar Arquivos ao Merge"),
            start,
            t("Arquivos Suportados (*.zip *.package *.7z *.rar);;Todos (*.*)"),
            options=QFileDialog.Option.DontUseNativeDialog,
        )
        files = [f for f in files if f.lower().endswith((".zip", ".7z", ".rar", ".package"))]
        if files:
            self.add_job(self.build_files_job(files))

    def refresh_jobs_tree(self):
        self.jobs_tree.clear()
        for index, job in enumerate(self.jobs):
            item = QTreeWidgetItem([
                job["name"],
                str(job.get("package_count", "?")),
                str(job.get("estimated_parts", "?")),
                t(POST_MODES[job.get("post_mode", "keep")]),
                self.status_label(job.get("status", "waiting")),
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, index)
            for col, alignment in enumerate(self.merge_column_alignments()):
                item.setTextAlignment(col, alignment)
            self.jobs_tree.addTopLevelItem(item)
        if self.jobs and not self.jobs_tree.selectedItems():
            self.jobs_tree.setCurrentItem(self.jobs_tree.topLevelItem(0))
        self.refresh_details()

    def status_label(self, status):
        return {
            "waiting": t("Aguardando"),
            "running": t("Rodando"),
            "done": t("Concluído"),
            "warning": t("Concluído com aviso"),
            "partial": t("Parcial"),
            "failed": t("Falhou"),
        }.get(status, status)

    def selected_job_index(self):
        items = self.jobs_tree.selectedItems()
        if not items:
            return None
        index = items[0].data(0, Qt.ItemDataRole.UserRole)
        return index if isinstance(index, int) and 0 <= index < len(self.jobs) else None

    def refresh_details(self):
        index = self.selected_job_index()
        if index is None:
            self.detail_label.setText(t("Nenhuma tarefa selecionada."))
            self.risk_list.clear()
            return
        job = self.jobs[index]
        self.post_mode_combo.blockSignals(True)
        self.limit_combo.blockSignals(True)
        mode_index = self.post_mode_combo.findData(job.get("post_mode", "keep"))
        self.post_mode_combo.setCurrentIndex(max(0, mode_index))
        limit_text = f"{job.get('max_size_gb', 1.0)} GB"
        limit_index = self.limit_combo.findText(limit_text)
        self.limit_combo.setCurrentIndex(max(0, limit_index))
        self.post_mode_combo.blockSignals(False)
        self.limit_combo.blockSignals(False)

        source = job["input_dir"] if job["source_type"] == "folder" else t("{} arquivo(s) selecionado(s)").format(len(job["files"]))
        self.detail_label.setText(
            t("<b>{}</b><br>Origem: {}<br>Destino: {}<br>Packages: {} | Tamanho: {} | Partes estimadas: {}").format(
                job["name"], source, job["output_dir"], job.get("package_count", "?"),
                format_size(job.get("size", 0)), job.get("estimated_parts", "?")
            )
        )
        self.risk_list.clear()
        risks = job.get("risks", [])
        if risks:
            for risk in risks:
                self.risk_list.addItem(QListWidgetItem("⚠️ " + risk))
        else:
            self.risk_list.addItem(QListWidgetItem(t("Nenhum sinal de risco detectado pelo nome ou estrutura.")))

    def on_mode_changed(self):
        index = self.selected_job_index()
        if index is None:
            return
        self.jobs[index]["post_mode"] = self.post_mode_combo.currentData()
        self.refresh_jobs_tree()

    def on_limit_changed(self):
        index = self.selected_job_index()
        if index is None:
            return
        value = float(self.limit_combo.currentText().split()[0])
        self.jobs[index]["max_size_gb"] = value
        if self.jobs[index]["source_type"] == "folder":
            count, parts = calculate_merge_stats(self.jobs[index]["input_dir"], value)
            self.jobs[index]["package_count"] = count
            self.jobs[index]["estimated_parts"] = parts
        self.refresh_jobs_tree()

    def on_remove_job(self):
        index = self.selected_job_index()
        if index is None:
            return
        self.jobs.pop(index)
        self.refresh_jobs_tree()

    def on_edit_destination(self):
        index = self.selected_job_index()
        if index is None:
            return
        start = get_mods_dir() or os.path.expanduser("~")
        folder = QFileDialog.getExistingDirectory(self, t("Escolha o Destino do Merge"), start, options=QFileDialog.Option.DontUseNativeDialog)
        if folder:
            if is_broad_path(folder):
                QMessageBox.warning(self, t("Destino inseguro"), t("Escolha uma subpasta específica para salvar o merge."))
                return
            self.jobs[index]["output_dir"] = folder
            self.refresh_jobs_tree()

    def validate_before_start(self):
        if not self.jobs:
            QMessageBox.information(self, t("Aviso"), t("Adicione pelo menos uma tarefa de merge."))
            return False
        for job in self.jobs:
            if not job["output_dir"] or is_broad_path(job["output_dir"]):
                QMessageBox.warning(self, t("Destino inválido"), t("Revise o destino da tarefa: {}").format(job["name"]))
                return False
            if job.get("post_mode") == "delete":
                text, ok = QInputDialog.getText(
                    self,
                    t("Confirmação Perigosa"),
                    t("A tarefa '{}' excluirá os originais após sucesso total.\nDigite EXCLUIR ORIGINAIS para confirmar:").format(job["name"]),
                )
                if not ok or text != "EXCLUIR ORIGINAIS":
                    return False
            if job.get("post_mode") in ("disable", "backup", "delete") and job.get("risks"):
                reply = QMessageBox.question(
                    self,
                    t("Risco Detectado"),
                    t("A tarefa '{}' tem sinais de mod sensível. Continuar mesmo assim?").format(job["name"]),
                    QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
                )
                if reply != QMessageBox.StandardButton.Yes:
                    return False
        return True

    def start_queue(self):
        if not self.validate_before_start():
            return
        self.btn_start.setEnabled(False)
        self.progress_bar.setValue(0)
        self.log_console.clear()
        for job in self.jobs:
            job["status"] = "waiting"
        self.refresh_jobs_tree()
        self.lbl_status.setText(t("Executando fila de merges..."))

        self.worker = MergeQueueWorker([dict(job) for job in self.jobs])
        self.worker.progress_signal.connect(self.update_progress)
        self.worker.log_signal.connect(self.append_log)
        self.worker.job_status_signal.connect(self.on_job_status)
        self.worker.finished_signal.connect(self.on_queue_finished)
        self.worker.start()

    def update_progress(self, current, total, status_text):
        if total > 0:
            self.progress_bar.setValue(int((current / total) * 100))
        if status_text:
            self.lbl_status.setText(status_text)

    def on_job_status(self, index, status):
        if 0 <= index < len(self.jobs):
            self.jobs[index]["status"] = status
            self.refresh_jobs_tree()

    def on_queue_finished(self, results):
        self.btn_start.setEnabled(True)
        done = sum(1 for result in results if result.get("status") in ("done", "warning"))
        failed = sum(1 for result in results if result.get("status") in ("failed", "partial"))
        self.lbl_status.setText(t("Fila concluída: {} concluída(s), {} com falha/parcial.").format(done, failed))
        QMessageBox.information(self, t("Fila de Merges"), self.lbl_status.text())
