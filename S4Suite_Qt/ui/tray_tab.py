from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QPushButton, QFileDialog,
    QGroupBox, QTextEdit, QMessageBox, QTreeWidget, QTreeWidgetItem,
    QSplitter, QAbstractItemView, QHeaderView, QInputDialog, QProgressBar,
    QCheckBox,
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal

from s4common import get_mods_dir, get_tray_dir, is_path_inside
from s4tray import analyze_tray_source, import_tray_plan, update_index
from s4translator import t

import os


class TrayAnalyzeWorker(QThread):
    log_signal = pyqtSignal(str)
    finished_signal = pyqtSignal(list)

    def __init__(self, sources, mods_dir):
        super().__init__()
        self.sources = sources
        self.mods_dir = mods_dir

    def run(self):
        self.log_signal.emit(t("🔄 Atualizando cache de mods para detectar duplicatas..."))
        update_index(self.mods_dir)
        results = []
        for index, source in enumerate(self.sources, start=1):
            self.log_signal.emit(t("🔎 Analisando {}/{}: {}").format(index, len(self.sources), os.path.basename(source)))
            result = analyze_tray_source(source, skip_index=True)
            results.append(result)
        self.finished_signal.emit(results)


class TrayImportWorker(QThread):
    log_signal = pyqtSignal(str)
    finished_signal = pyqtSignal(list)

    def __init__(self, plans, mods_dir):
        super().__init__()
        self.plans = plans
        self.mods_dir = mods_dir

    def run(self):
        self.log_signal.emit(t("🔄 Atualizando cache antes da importação..."))
        update_index(self.mods_dir)
        results = []
        for index, plan in enumerate(self.plans, start=1):
            name = plan.get("import_name") or os.path.basename(plan.get("source_path", ""))
            self.log_signal.emit(t("\n📦 Importando {}/{}: {}").format(index, len(self.plans), name))
            success, message, stats = import_tray_plan(plan, skip_index=True, log_callback=self.log_signal.emit)
            results.append({"success": success, "message": message, "stats": stats or {}, "name": name})
            if success:
                self.log_signal.emit(t("  ↳ {} Tray | {} CC | {} duplicata(s) pulada(s) | {} ignorado(s)").format(
                    stats.get("tray", 0), stats.get("installed", 0), stats.get("duplicates", 0), stats.get("skipped", 0)
                ))
            else:
                self.log_signal.emit(t("  ↳ Erro: {}").format(message))
        self.finished_signal.emit(results)


class TrayTab(QWidget):
    def __init__(self):
        super().__init__()
        self.sources = []
        self.analyses = []
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
        files = [url.toLocalFile() for url in event.mimeData().urls()]
        self.add_sources(files)

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(12)

        title = QLabel(t("🗂️ Importador de Sims/Lotes"))
        title.setObjectName("HeaderTitle")
        layout.addWidget(title)

        desc = QLabel(t("Analise Sims e lotes antes de importar, revise o CC incluído e escolha exatamente onde cada package será instalado."))
        desc.setObjectName("SubDescription")
        desc.setWordWrap(True)
        layout.addWidget(desc)

        file_row = QHBoxLayout()
        self.btn_browse = QPushButton(t("📁 Selecionar Arquivos"))
        self.btn_browse.setObjectName("primary")
        self.btn_browse.clicked.connect(self.on_browse_clicked)
        file_row.addWidget(self.btn_browse)

        self.btn_analyze = QPushButton(t("Analisar Conteúdo"))
        self.btn_analyze.clicked.connect(self.on_analyze_clicked)
        file_row.addWidget(self.btn_analyze)

        self.btn_clear = QPushButton(t("Limpar Fila"))
        self.btn_clear.clicked.connect(self.clear_all)
        file_row.addWidget(self.btn_clear)
        layout.addLayout(file_row)

        splitter = QSplitter(Qt.Orientation.Horizontal)
        layout.addWidget(splitter, stretch=3)

        queue_group = QGroupBox(t("Fila de Sims/Lotes"))
        queue_layout = QVBoxLayout(queue_group)
        self.queue_tree = QTreeWidget()
        self.queue_tree.setHeaderLabels([t("Item"), t("Tray"), t("CC"), t("Duplicatas"), t("Status")])
        self.queue_tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col in (1, 2, 3, 4):
            self.queue_tree.header().setSectionResizeMode(col, QHeaderView.ResizeMode.ResizeToContents)
        self.queue_tree.setRootIsDecorated(False)
        self.queue_tree.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self.queue_tree.itemSelectionChanged.connect(self.refresh_content_view)
        queue_layout.addWidget(self.queue_tree)
        splitter.addWidget(queue_group)

        content_group = QGroupBox(t("Revisão do Conteúdo"))
        content_layout = QVBoxLayout(content_group)
        self.summary_label = QLabel(t("Selecione e analise arquivos para revisar o conteúdo."))
        self.summary_label.setWordWrap(True)
        content_layout.addWidget(self.summary_label)

        mode_row = QHBoxLayout()
        self.chk_install_tray = QCheckBox(t("Instalar arquivos Tray"))
        self.chk_install_tray.setChecked(True)
        self.chk_install_tray.toggled.connect(lambda _checked: self.refresh_content_view())
        mode_row.addWidget(self.chk_install_tray)

        self.chk_install_cc = QCheckBox(t("Instalar Mods/CC"))
        self.chk_install_cc.setChecked(True)
        self.chk_install_cc.toggled.connect(lambda _checked: self.refresh_content_view())
        mode_row.addWidget(self.chk_install_cc)
        mode_row.addStretch()
        content_layout.addLayout(mode_row)

        tray_group = QGroupBox(t("Arquivos Tray do Sim/Lote"))
        tray_group.setMinimumHeight(230)
        tray_group.setMaximumHeight(285)
        tray_layout = QVBoxLayout(tray_group)
        self.tray_tree = QTreeWidget()
        self.tray_tree.setHeaderLabels([t("Arquivo"), t("Tamanho"), t("Destino")])
        self.tray_tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col in (1, 2):
            self.tray_tree.header().setSectionResizeMode(col, QHeaderView.ResizeMode.ResizeToContents)
        self.tray_tree.setRootIsDecorated(False)
        self.tray_tree.setSelectionMode(QAbstractItemView.SelectionMode.NoSelection)
        self.tray_tree.setMinimumHeight(160)
        self.tray_tree.setMaximumHeight(220)
        tray_layout.addWidget(self.tray_tree)
        content_layout.addWidget(tray_group)

        mods_group = QGroupBox(t("Mods/CC Incluídos"))
        mods_layout = QVBoxLayout(mods_group)
        self.mods_tree = QTreeWidget()
        self.mods_tree.setHeaderLabels([t("Arquivo"), t("Tamanho"), t("Status"), t("Destino/Ação")])
        self.mods_tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col in (1, 2, 3):
            self.mods_tree.header().setSectionResizeMode(col, QHeaderView.ResizeMode.ResizeToContents)
        self.mods_tree.setTextElideMode(Qt.TextElideMode.ElideMiddle)
        self.mods_tree.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.mods_tree.setRootIsDecorated(False)
        self.mods_tree.setMinimumHeight(220)
        mods_layout.addWidget(self.mods_tree)

        content_actions = QHBoxLayout()
        btn_apply = QPushButton(t("Aplicar Destino"))
        btn_apply.clicked.connect(self.apply_destination_to_selected)
        content_actions.addWidget(btn_apply)

        btn_apply_all = QPushButton(t("Aplicar Destino a Todos"))
        btn_apply_all.clicked.connect(self.apply_destination_to_all)
        content_actions.addWidget(btn_apply_all)

        btn_skip = QPushButton(t("Ignorar Selecionados"))
        btn_skip.clicked.connect(self.skip_selected_packages)
        content_actions.addWidget(btn_skip)

        btn_install = QPushButton(t("Instalar Selecionados"))
        btn_install.clicked.connect(self.install_selected_packages)
        content_actions.addWidget(btn_install)
        mods_layout.addLayout(content_actions)
        content_layout.addWidget(mods_group, stretch=1)
        splitter.addWidget(content_group)

        folders_group = QGroupBox(t("Destinos em Mods"))
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
        self.folder_tree.itemSelectionChanged.connect(self.on_folder_selected)
        folders_layout.addWidget(self.folder_tree)

        self.selected_folder_label = QLabel(t("Selecione uma pasta dentro de Mods."))
        self.selected_folder_label.setWordWrap(True)
        folders_layout.addWidget(self.selected_folder_label)
        splitter.addWidget(folders_group)
        splitter.setSizes([260, 760, 300])

        self.progress_bar = QProgressBar()
        self.progress_bar.setValue(0)
        layout.addWidget(self.progress_bar)

        bottom_row = QHBoxLayout()
        self.btn_import = QPushButton(t("Importar Conteúdo Revisado"))
        self.btn_import.setObjectName("primary")
        self.btn_import.clicked.connect(self.on_import_clicked)
        bottom_row.addWidget(self.btn_import)
        layout.addLayout(bottom_row)

        self.log_console = QTextEdit()
        self.log_console.setReadOnly(True)
        self.log_console.setMaximumHeight(180)
        layout.addWidget(self.log_console)

    def valid_sources(self, paths):
        valid = []
        for path in paths:
            if os.path.isdir(path) or path.lower().endswith((".zip", ".7z", ".rar")):
                valid.append(path)
        return valid

    def add_sources(self, paths):
        added = 0
        for path in self.valid_sources(paths):
            if path not in self.sources:
                self.sources.append(path)
                added += 1
        if added:
            self.append_log(t("➕ {} item(ns) adicionado(s) para análise.").format(added))
        else:
            self.append_log(t("⚠️ Nenhum arquivo compactado ou pasta válida foi adicionado."))
        self.refresh_queue()

    def on_browse_clicked(self):
        downloads_dir = os.path.expanduser("~/Downloads")
        files, _ = QFileDialog.getOpenFileNames(
            self,
            t("Selecione arquivos de Sim/Lote"),
            downloads_dir,
            t("Arquivos Compactados (*.zip *.7z *.rar);;Todos (*.*)"),
            options=QFileDialog.Option.DontUseNativeDialog,
        )
        self.add_sources(files)

    def clear_all(self):
        self.sources = []
        self.analyses = []
        self.tray_tree.clear()
        self.mods_tree.clear()
        self.summary_label.setText(t("Selecione e analise arquivos para revisar o conteúdo."))
        self.refresh_queue()

    def on_analyze_clicked(self):
        mods_dir = get_mods_dir()
        tray_dir = get_tray_dir()
        if not self.sources or not mods_dir or not tray_dir:
            QMessageBox.warning(self, t("Aviso"), t("Configure as pastas do The Sims 4 e selecione pelo menos um arquivo."))
            return

        self.set_working(True)
        self.progress_bar.setValue(0)
        self.log_console.clear()
        self.analyze_worker = TrayAnalyzeWorker(list(self.sources), mods_dir)
        self.analyze_worker.log_signal.connect(self.append_log)
        self.analyze_worker.finished_signal.connect(self.on_analysis_finished)
        self.analyze_worker.start()

    def on_analysis_finished(self, results):
        self.analyses = []
        for result in results:
            if result.get("success"):
                self.prepare_analysis_defaults(result)
                self.append_log(t("✅ Análise concluída: {}").format(result.get("source_name", "")))
            else:
                self.append_log(t("❌ Falha na análise de {}: {}").format(result.get("source_name", ""), result.get("error", "")))
            self.analyses.append(result)
        self.refresh_queue()
        self.refresh_content_view()
        self.progress_bar.setValue(100)
        self.set_working(False)

    def prepare_analysis_defaults(self, analysis):
        for package in analysis.get("package_files", []):
            package["target_dir"] = analysis["default_target_dir"]
            package["action"] = "skip" if package.get("duplicate_path") else "install"

    def refresh_queue(self):
        self.queue_tree.clear()
        if self.analyses:
            for index, analysis in enumerate(self.analyses):
                if analysis.get("success"):
                    duplicates = len([p for p in analysis.get("package_files", []) if p.get("duplicate_path")])
                    item = QTreeWidgetItem([
                        analysis.get("import_name", analysis.get("source_name", "")),
                        str(len(analysis.get("tray_files", []))),
                        str(len(analysis.get("package_files", []))),
                        str(duplicates),
                        t("Revisado"),
                    ])
                else:
                    item = QTreeWidgetItem([
                        analysis.get("source_name", os.path.basename(analysis.get("source_path", ""))),
                        "0",
                        "0",
                        "0",
                        t("Erro"),
                    ])
                item.setData(0, Qt.ItemDataRole.UserRole, index)
                self.queue_tree.addTopLevelItem(item)
            if self.queue_tree.topLevelItemCount() and not self.queue_tree.selectedItems():
                self.queue_tree.setCurrentItem(self.queue_tree.topLevelItem(0))
            return

        for index, source in enumerate(self.sources):
            item = QTreeWidgetItem([os.path.basename(source), "-", "-", "-", t("Aguardando análise")])
            item.setData(0, Qt.ItemDataRole.UserRole, index)
            self.queue_tree.addTopLevelItem(item)

    def selected_analysis_index(self):
        selected = self.queue_tree.selectedItems()
        if not selected:
            return None
        index = selected[0].data(0, Qt.ItemDataRole.UserRole)
        if isinstance(index, int) and 0 <= index < len(self.analyses):
            return index
        return None

    def refresh_content_view(self):
        self.tray_tree.clear()
        self.mods_tree.clear()
        index = self.selected_analysis_index()
        if index is None:
            return

        analysis = self.analyses[index]
        if not analysis.get("success"):
            self.summary_label.setText(t("Falha na análise: {}").format(analysis.get("error", "")))
            return

        warnings = analysis.get("warnings", [])
        summary = t("<b>{}</b><br>Tray: {} | CC: {} | Outros ignorados: {} | Bloqueados: {}").format(
            analysis.get("import_name"),
            len(analysis.get("tray_files", [])),
            len(analysis.get("package_files", [])),
            len(analysis.get("other_files", [])),
            len(analysis.get("blocked_files", [])),
        )
        if warnings:
            summary += "<br>" + "<br>".join(warnings)
        self.summary_label.setText(summary)

        for tray_file in analysis.get("tray_files", []):
            item = QTreeWidgetItem([
                tray_file["rel_path"],
                tray_file["size_label"],
                t("Pasta Tray") if self.chk_install_tray.isChecked() else t("Não instalar"),
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, {"kind": "tray"})
            self.tray_tree.addTopLevelItem(item)

        for package in analysis.get("package_files", []):
            action_label = self.package_action_label(package)
            if not self.chk_install_cc.isChecked():
                action_label = t("Não instalar")
            item = QTreeWidgetItem([
                package["rel_path"],
                package["size_label"],
                self.package_status_label(package),
                action_label,
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, {"kind": "package", "rel_path": package["rel_path"]})
            self.mods_tree.addTopLevelItem(item)

        for blocked in analysis.get("blocked_files", []):
            item = QTreeWidgetItem([
                blocked["rel_path"],
                blocked["size_label"],
                t("Não será instalado"),
                t("Extensão perigosa"),
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, {"kind": "blocked"})
            self.mods_tree.addTopLevelItem(item)

        for other in analysis.get("other_files", []):
            item = QTreeWidgetItem([
                other["rel_path"],
                other["size_label"],
                t("Ignorado"),
                t("Não necessário para instalação"),
            ])
            item.setData(0, Qt.ItemDataRole.UserRole, {"kind": "other"})
            self.mods_tree.addTopLevelItem(item)

    def package_status_label(self, package):
        if package.get("duplicate_path"):
            return t("Duplicata exata")
        return t("Novo")

    def package_action_label(self, package):
        action = package.get("action", "install")
        if action == "skip":
            return t("Ignorar")
        target = package.get("target_dir")
        mods_dir = get_mods_dir()
        if target and mods_dir:
            rel = os.path.relpath(target, mods_dir)
            label = "Mods" if rel == "." else f"Mods/{rel}"
            if action == "install_copy":
                return t("Instalar cópia em {}").format(label)
            if package.get("duplicate_path"):
                return t("Duplicata será pulada")
            return t("Instalar em {}").format(label)
        return t("Instalar")

    def selected_packages(self):
        index = self.selected_analysis_index()
        if index is None:
            return []
        analysis = self.analyses[index]
        packages_by_rel = {pkg["rel_path"]: pkg for pkg in analysis.get("package_files", [])}
        selected = []
        for item in self.mods_tree.selectedItems():
            data = item.data(0, Qt.ItemDataRole.UserRole) or {}
            if data.get("kind") == "package" and data.get("rel_path") in packages_by_rel:
                selected.append(packages_by_rel[data["rel_path"]])
        return selected

    def apply_destination_to_selected(self):
        if not self.selected_target_dir:
            QMessageBox.warning(self, t("Destino"), t("Selecione uma pasta de destino em Mods."))
            return
        selected = self.selected_packages()
        if not selected:
            return
        for package in selected:
            package["target_dir"] = self.selected_target_dir
            if not package.get("duplicate_path"):
                package["action"] = "install"
        self.refresh_content_view()

    def apply_destination_to_all(self):
        if not self.selected_target_dir:
            QMessageBox.warning(self, t("Destino"), t("Selecione uma pasta de destino em Mods."))
            return
        index = self.selected_analysis_index()
        if index is None:
            return
        for package in self.analyses[index].get("package_files", []):
            package["target_dir"] = self.selected_target_dir
            if not package.get("duplicate_path"):
                package["action"] = "install"
        self.refresh_content_view()

    def skip_selected_packages(self):
        for package in self.selected_packages():
            package["action"] = "skip"
        self.refresh_content_view()

    def install_selected_packages(self):
        for package in self.selected_packages():
            package["action"] = "install_copy" if package.get("duplicate_path") else "install"
        self.refresh_content_view()

    def load_folders(self):
        mods_dir = get_mods_dir()
        self.folder_tree.clear()
        self.folder_items = {}
        if not mods_dir or not os.path.isdir(mods_dir):
            return

        root_item = QTreeWidgetItem([t("Mods")])
        root_item.setData(0, Qt.ItemDataRole.UserRole, mods_dir)
        self.folder_tree.addTopLevelItem(root_item)
        self.folder_items[mods_dir] = root_item

        def add_children(parent_item, path):
            try:
                entries = sorted((entry for entry in os.scandir(path) if entry.is_dir()), key=lambda entry: entry.name.lower())
            except Exception as e:
                self.append_log(t("⚠️ Falha ao ler pasta {}: {}").format(path, e))
                return
            for entry in entries:
                item = QTreeWidgetItem([entry.name])
                item.setData(0, Qt.ItemDataRole.UserRole, entry.path)
                item.setToolTip(0, entry.path)
                parent_item.addChild(item)
                self.folder_items[entry.path] = item
                add_children(item, entry.path)

        add_children(root_item, mods_dir)
        root_item.setExpanded(True)
        self.folder_tree.setCurrentItem(root_item)

    def on_folder_selected(self):
        selected = self.folder_tree.selectedItems()
        self.selected_target_dir = selected[0].data(0, Qt.ItemDataRole.UserRole) if selected else None
        mods_dir = get_mods_dir()
        if self.selected_target_dir and mods_dir:
            rel = os.path.relpath(self.selected_target_dir, mods_dir)
            label = "Mods" if rel == "." else f"Mods/{rel}"
            self.selected_folder_label.setText(t("Destino selecionado: {}").format(label))
        else:
            self.selected_folder_label.setText(t("Selecione uma pasta dentro de Mods."))

    def on_create_folder(self):
        mods_dir = get_mods_dir()
        if not mods_dir:
            return
        parent_dir = self.selected_target_dir or mods_dir
        parent_real = os.path.realpath(parent_dir)
        mods_real = os.path.realpath(mods_dir)
        if parent_real != mods_real and not is_path_inside(parent_real, mods_real):
            QMessageBox.warning(self, t("Destino bloqueado"), t("A nova pasta precisa ficar dentro de Mods."))
            return

        name, ok = QInputDialog.getText(self, t("Nova Pasta"), t("Nome da nova pasta:"))
        if not ok or not name.strip():
            return
        safe_name = "".join(c for c in name.strip() if c not in '/\\:*?"<>|').strip()
        if not safe_name:
            QMessageBox.warning(self, t("Nome inválido"), t("Escolha um nome de pasta válido."))
            return
        try:
            os.makedirs(os.path.join(parent_dir, safe_name), exist_ok=False)
            self.load_folders()
        except FileExistsError:
            QMessageBox.warning(self, t("Pasta já existe"), t("Já existe uma pasta com esse nome."))
        except Exception as e:
            QMessageBox.critical(self, t("Erro"), t("Falha ao criar pasta: {}").format(e))

    def build_import_plans(self):
        plans = []
        for analysis in self.analyses:
            if not analysis.get("success"):
                continue
            plan = {
                "source_path": analysis["source_path"],
                "import_name": analysis["import_name"],
                "default_target_dir": analysis["default_target_dir"],
                "tray_files": [item["rel_path"] for item in analysis.get("tray_files", [])] if self.chk_install_tray.isChecked() else [],
                "package_files": [
                    {
                        "rel_path": package["rel_path"],
                        "target_dir": package.get("target_dir") or analysis["default_target_dir"],
                        "action": package.get("action", "install"),
                    }
                    for package in analysis.get("package_files", [])
                ] if self.chk_install_cc.isChecked() else [],
            }
            if plan["tray_files"] or plan["package_files"]:
                plans.append(plan)
        return plans

    def on_import_clicked(self):
        mods_dir = get_mods_dir()
        tray_dir = get_tray_dir()
        if not self.analyses or not mods_dir or not tray_dir:
            QMessageBox.warning(self, t("Aviso"), t("Analise pelo menos um Sim/Lote antes de importar."))
            return
        if not self.chk_install_tray.isChecked() and not self.chk_install_cc.isChecked():
            QMessageBox.warning(self, t("Aviso"), t("Escolha instalar arquivos Tray, Mods/CC ou ambos."))
            return

        plans = self.build_import_plans()
        if not plans:
            QMessageBox.warning(self, t("Aviso"), t("Nenhum plano válido para importar."))
            return

        total_packages = sum(len(plan["package_files"]) for plan in plans)
        total_tray = sum(len(plan["tray_files"]) for plan in plans)
        skipped = sum(1 for plan in plans for package in plan["package_files"] if package.get("action") == "skip")
        reply = QMessageBox.question(
            self,
            t("Confirmar Importação"),
            t("Importar {} item(ns)? Tray: {} | CC listados: {} | ignorados: {}.").format(len(plans), total_tray, total_packages, skipped),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply != QMessageBox.StandardButton.Yes:
            return

        self.set_working(True)
        self.progress_bar.setValue(0)
        self.import_worker = TrayImportWorker(plans, mods_dir)
        self.import_worker.log_signal.connect(self.append_log)
        self.import_worker.finished_signal.connect(self.on_import_finished)
        self.import_worker.start()

    def on_import_finished(self, results):
        self.progress_bar.setValue(100)
        self.set_working(False)
        success_count = sum(1 for result in results if result.get("success"))
        fail_count = len(results) - success_count
        QMessageBox.information(
            self,
            t("Importação"),
            t("Importação concluída: {} sucesso(s), {} falha(s).").format(success_count, fail_count),
        )

    def set_working(self, working):
        self.btn_import.setEnabled(not working)
        self.btn_browse.setEnabled(not working)
        self.btn_analyze.setEnabled(not working)
        self.btn_clear.setEnabled(not working)

    def append_log(self, text):
        self.log_console.append(text)
        scrollbar = self.log_console.verticalScrollBar()
        scrollbar.setValue(scrollbar.maximum())
