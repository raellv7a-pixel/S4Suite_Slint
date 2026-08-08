from PyQt6.QtWidgets import (
    QWidget, QVBoxLayout, QHBoxLayout, QLabel, QPushButton, 
    QTreeWidget, QTreeWidgetItem, QCheckBox, QMessageBox, 
    QDialog, QLineEdit, QMenu, QListWidget, QListWidgetItem, QAbstractItemView,
    QHeaderView, QInputDialog
)
from PyQt6.QtCore import Qt, QThread, pyqtSignal, QTimer
from PyQt6.QtGui import QAction, QIcon
from s4common import ConfigManager, get_mods_dir, safe_delete_folder, safe_remove, unique_dest_path
from s4disabled import (
    REASONS,
    delete_disabled,
    disable_mod,
    list_disabled_mods,
    register_existing_disabled,
    restore_disabled,
)
import os
import shutil
import hashlib
from s4translator import t

# ====================================================================
# WORKERS (Processamento em 2º plano)
# ====================================================================

class TreeLoaderWorker(QThread):
    finished_signal = pyqtSignal(dict, list)
    
    def __init__(self, mods_dir):
        super().__init__()
        self.mods_dir = mods_dir
        
    def run(self):
        errors = []
        def build_tree(path):
            node = {'name': os.path.basename(path) or path, 'path': path, 'type': 'dir', 'children': []}
            try:
                for entry in sorted(os.scandir(path), key=lambda e: (not e.is_dir(), e.name.lower())):
                    if entry.is_dir():
                        node['children'].append(build_tree(entry.path))
                    else:
                        # Oculta arquivos ocultos de sistema para não poluir
                        if not entry.name.startswith('.'):
                            node['children'].append({'name': entry.name, 'path': entry.path, 'type': 'file', 'size': entry.stat().st_size})
            except Exception as e:
                errors.append(t("{}: {}").format(path, e))
            return node
            
        tree = build_tree(self.mods_dir)
        self.finished_signal.emit(tree, errors)

class DuplicateWorker(QThread):
    finished_signal = pyqtSignal(dict, list)
    
    def __init__(self, mods_dir):
        super().__init__()
        self.mods_dir = mods_dir
        
    def run(self):
        duplicates = {}
        file_map = {}
        errors = []
        
        if not self.mods_dir or not os.path.exists(self.mods_dir):
            self.finished_signal.emit({}, [])
            return
            
        for root, _, files in os.walk(self.mods_dir):
            for f in files:
                if f.lower().endswith(('.package', '.ts4script')):
                    path = os.path.join(root, f)
                    try:
                        size = os.path.getsize(path)
                        key = (size, f.lower())
                        if key in file_map:
                            file_map[key].append(path)
                        else:
                            file_map[key] = [path]
                    except Exception as e:
                        errors.append(t("{}: {}").format(path, e))
                        
        for key, paths in file_map.items():
            if len(paths) > 1:
                by_hash = {}
                for path in paths:
                    file_hash = self._sha256(path)
                    if file_hash:
                        by_hash.setdefault(file_hash, []).append(path)
                    else:
                        errors.append(t("Falha ao confirmar hash: {}").format(path))
                for file_hash, hashed_paths in by_hash.items():
                    if len(hashed_paths) > 1:
                        duplicates[(key[0], key[1], file_hash)] = hashed_paths
                
        self.finished_signal.emit(duplicates, errors)

    def _sha256(self, path):
        digest = hashlib.sha256()
        try:
            with open(path, "rb") as src:
                for chunk in iter(lambda: src.read(1024 * 1024), b""):
                    digest.update(chunk)
            return digest.hexdigest()
        except Exception as e:
            print(t("Erro ao confirmar duplicata {}: {}").format(path, e))
            return None

class JunkCleanerWorker(QThread):
    finished_signal = pyqtSignal(int, int, list) # files, folders, failures
    
    def __init__(self, mods_dir, delete_images):
        super().__init__()
        self.mods_dir = mods_dir
        self.delete_images = delete_images
        
    def run(self):
        junk_exts = ['.txt', '.url', '.log', '.htm', '.html', '.rtf']
        if self.delete_images:
            junk_exts.extend(['.jpg', '.jpeg', '.png', '.bmp', '.gif'])
            
        f_del = 0
        d_del = 0
        failed = []
        
        for root, dirs, files in os.walk(self.mods_dir):
            for f in files:
                ext = os.path.splitext(f)[1].lower()
                if ext in junk_exts or f.lower() in ['thumbs.db', '.ds_store']:
                    try:
                        if safe_remove(os.path.join(root, f)):
                            f_del += 1
                        else:
                            failed.append(os.path.join(root, f))
                    except Exception as e:
                        failed.append(t("{}: {}").format(os.path.join(root, f), e))

        # Deletar pastas vazias de baixo para cima
        for root, dirs, files in os.walk(self.mods_dir, topdown=False):
            if root == self.mods_dir: continue
            if not os.listdir(root):
                try:
                    os.rmdir(root)
                    d_del += 1
                except Exception as e:
                    failed.append(t("{}: {}").format(root, e))
                
        self.finished_signal.emit(f_del, d_del, failed)

class ScriptCheckerWorker(QThread):
    finished_signal = pyqtSignal(list)
    def __init__(self, mods_dir):
        super().__init__()
        self.mods_dir = mods_dir
        
    def run(self):
        bad_scripts = []
        mods_dir_normalized = os.path.normpath(self.mods_dir)
        
        for root, _, files in os.walk(self.mods_dir):
            for f in files:
                if f.lower().endswith('.ts4script'):
                    rel_path = os.path.relpath(root, mods_dir_normalized)
                    depth = 0 if rel_path == '.' else len(rel_path.split(os.sep))
                        
                    if depth > 1:
                        bad_scripts.append(os.path.join(root, f))
                        
        self.finished_signal.emit(bad_scripts)


# ====================================================================
# DIÁLOGOS
# ====================================================================

class DuplicatesDialog(QDialog):
    def __init__(self, duplicates_dict, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Detector de Duplicatas"))
        self.resize(700, 500)
        
        layout = QVBoxLayout(self)
        
        lbl_info = QLabel(t("Foram encontrados {} grupos de arquivos duplicados.").format(len(duplicates_dict)))
        lbl_info.setStyleSheet("font-size: 16px; font-weight: bold; color: #d97736;")
        layout.addWidget(lbl_info)
        
        lbl_desc = QLabel(t("Marque os arquivos que deseja EXCLUIR. Recomendamos manter pelo menos uma cópia desmarcada de cada grupo."))
        lbl_desc.setWordWrap(True)
        layout.addWidget(lbl_desc)
        
        self.list_widget = QListWidget()
        layout.addWidget(self.list_widget)
        
        self.check_items = []
        mods_dir = ConfigManager.get("sims4_path") + "/Mods" if ConfigManager.get("sims4_path") else ""
        
        for key, paths in duplicates_dict.items():
            size, name = key[0], key[1]
            
            header_item = QListWidgetItem(f"📦 {name} ({size / (1024*1024):.2f} MB)")
            header_item.setBackground(Qt.GlobalColor.darkGray)
            header_item.setForeground(Qt.GlobalColor.white)
            header_item.setFlags(Qt.ItemFlag.NoItemFlags)
            self.list_widget.addItem(header_item)
            
            for i, p in enumerate(paths):
                safe_p = p.encode('utf-8', 'replace').decode('utf-8')
                display_p = safe_p.replace(mods_dir, "Mods") if mods_dir and safe_p.startswith(mods_dir) else safe_p
                    
                item = QListWidgetItem()
                widget = QWidget()
                row = QHBoxLayout(widget)
                row.setContentsMargins(20, 0, 0, 0)
                
                chk = QCheckBox(display_p)
                if i > 0: chk.setChecked(True)
                
                row.addWidget(chk)
                row.addStretch()
                
                item.setSizeHint(widget.sizeHint())
                self.list_widget.addItem(item)
                self.list_widget.setItemWidget(item, widget)
                
                self.check_items.append((chk, p))
                
        btn_layout = QHBoxLayout()
        btn_delete = QPushButton(t("🗑️ Excluir Selecionados"))
        btn_delete.setObjectName("danger")
        btn_delete.clicked.connect(self.on_delete)
        btn_layout.addWidget(btn_delete)
        
        btn_cancel = QPushButton(t("Cancelar"))
        btn_cancel.clicked.connect(self.reject)
        btn_layout.addWidget(btn_cancel)
        
        layout.addLayout(btn_layout)
        
    def on_delete(self):
        paths_to_delete = [path for chk, path in self.check_items if chk.isChecked()]
        if not paths_to_delete:
            QMessageBox.information(self, t("Aviso"), t("Nenhum arquivo selecionado."))
            return
            
        reply = QMessageBox.question(self, t("Confirmar Exclusão"), t("Tem certeza que deseja apagar {} arquivo(s)?").format(len(paths_to_delete)), QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No)
        
        if reply == QMessageBox.StandardButton.Yes:
            deleted = []
            failed = []
            for path in paths_to_delete:
                if safe_remove(path):
                    deleted.append(path)
                else:
                    failed.append(path)
            msg = t("{} arquivos apagados.").format(len(deleted))
            if failed:
                msg += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
            QMessageBox.information(self, t("Sucesso"), msg)
            self.accept()

class BadScriptsDialog(QDialog):
    def __init__(self, bad_scripts, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Scripts Muito Profundos Encontrados"))
        self.resize(600, 400)
        self.bad_scripts = bad_scripts
        
        layout = QVBoxLayout(self)
        
        lbl_info = QLabel(t("<b>Aviso Crítico:</b> O The Sims 4 NÃO consegue ler arquivos <code>.ts4script</code> se eles estiverem a mais de UMA pasta de profundidade dentro da pasta Mods."))
        lbl_info.setWordWrap(True)
        lbl_info.setStyleSheet("color: #d9534f;")
        layout.addWidget(lbl_info)
        
        self.list_widget = QListWidget()
        mods_dir = ConfigManager.get("sims4_path") + "/Mods" if ConfigManager.get("sims4_path") else ""
        
        for script in self.bad_scripts:
            display_p = script.replace(mods_dir, "Mods") if mods_dir and script.startswith(mods_dir) else script
            self.list_widget.addItem(QListWidgetItem(display_p))
            
        layout.addWidget(self.list_widget)
        
        lbl_tip = QLabel(t("<i>Recomendação: Mova esses arquivos para uma pasta mais rasa (ex: Mods/Scripts). Você pode fazer isso manualmente pela árvore.</i>"))
        lbl_tip.setWordWrap(True)
        layout.addWidget(lbl_tip)
        
        btn_row = QHBoxLayout()
        btn_fix = QPushButton(t("🔧 Corrigir Automaticamente"))
        btn_fix.setObjectName("primary")
        btn_fix.clicked.connect(self.on_auto_fix)
        btn_row.addWidget(btn_fix)
        
        btn_close = QPushButton(t("Entendido"))
        btn_close.clicked.connect(self.reject)
        btn_row.addWidget(btn_close)
        
        layout.addLayout(btn_row)

    def on_auto_fix(self):
        mods_dir = ConfigManager.get("sims4_path") + "/Mods" if ConfigManager.get("sims4_path") else ""
        if not mods_dir: return
        
        target_dir = os.path.join(mods_dir, "00_Scripts_Corrigidos")
        os.makedirs(target_dir, exist_ok=True)
        
        count = 0
        failed = []
        for script in self.bad_scripts:
            if not os.path.exists(script): continue
            dest = unique_dest_path(target_dir, os.path.basename(script))
            try:
                shutil.move(script, dest)
                count += 1
            except Exception as e:
                failed.append(f"{script}: {e}")

        msg = t("{} scripts foram movidos para a raiz da pasta Mods (00_Scripts_Corrigidos) e agora serão lidos pelo jogo.").format(count)
        if failed:
            msg += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Correção Aplicada"), msg)
        self.accept()

class FolderPickerDialog(QDialog):
    def __init__(self, mods_dir, title, parent=None):
        super().__init__(parent)
        self.mods_dir = mods_dir
        self.selected_folder = None
        self.setWindowTitle(title)
        self.resize(520, 460)

        layout = QVBoxLayout(self)
        lbl = QLabel(t("Escolha uma pasta dentro de Mods. Operações fora de Mods são bloqueadas por segurança."))
        lbl.setWordWrap(True)
        layout.addWidget(lbl)

        self.tree = QTreeWidget()
        self.tree.setHeaderHidden(True)
        self.tree.setTextElideMode(Qt.TextElideMode.ElideNone)
        self.tree.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.tree.setIndentation(18)
        layout.addWidget(self.tree)

        row = QHBoxLayout()
        btn_ok = QPushButton(t("Escolher Pasta"))
        btn_ok.setObjectName("primary")
        btn_ok.clicked.connect(self.on_accept)
        btn_cancel = QPushButton(t("Cancelar"))
        btn_cancel.clicked.connect(self.reject)
        row.addWidget(btn_ok)
        row.addWidget(btn_cancel)
        layout.addLayout(row)

        self.populate()

    def populate(self):
        self.tree.clear()

        def add_children(parent_item, path):
            try:
                entries = sorted((e for e in os.scandir(path) if e.is_dir()), key=lambda e: e.name.lower())
            except Exception:
                return
            for entry in entries:
                item = QTreeWidgetItem([entry.name])
                item.setData(0, Qt.ItemDataRole.UserRole, entry.path)
                item.setToolTip(0, entry.path)
                parent_item.addChild(item)
                add_children(item, entry.path)

        root = QTreeWidgetItem([t("Mods")])
        root.setData(0, Qt.ItemDataRole.UserRole, self.mods_dir)
        self.tree.addTopLevelItem(root)
        add_children(root, self.mods_dir)
        root.setExpanded(True)
        self.tree.setCurrentItem(root)

    def on_accept(self):
        item = self.tree.currentItem()
        if not item:
            return
        self.selected_folder = item.data(0, Qt.ItemDataRole.UserRole)
        self.accept()

class DisabledModsDialog(QDialog):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle(t("Mods Desativados"))
        self.resize(900, 560)
        layout = QVBoxLayout(self)

        lbl = QLabel(t("Revise mods desativados pelo S4Suite. Você pode restaurar ou excluir arquivos desativados para recuperar espaço."))
        lbl.setWordWrap(True)
        layout.addWidget(lbl)

        self.tree = QTreeWidget()
        self.tree.setObjectName("DisabledModsTree")
        self.tree.setHeaderLabels([t("Arquivo"), t("Motivo"), t("Origem"), t("Data"), t("Existe")])
        self.tree.header().setMinimumHeight(36)
        self.tree.header().setDefaultAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        self.tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col in (1, 2, 3, 4):
            self.tree.header().setSectionResizeMode(col, QHeaderView.ResizeMode.ResizeToContents)
        self.tree.setTextElideMode(Qt.TextElideMode.ElideNone)
        self.tree.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.tree.setUniformRowHeights(True)
        self.tree.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        layout.addWidget(self.tree)

        row = QHBoxLayout()
        btn_refresh = QPushButton(t("Recarregar"))
        btn_refresh.clicked.connect(self.load_items)
        row.addWidget(btn_refresh)

        btn_restore = QPushButton(t("Restaurar Selecionados"))
        btn_restore.setObjectName("primary")
        btn_restore.clicked.connect(self.on_restore)
        row.addWidget(btn_restore)

        btn_delete = QPushButton(t("Excluir Desativados"))
        btn_delete.setObjectName("danger")
        btn_delete.clicked.connect(self.on_delete)
        row.addWidget(btn_delete)

        btn_close = QPushButton(t("Fechar"))
        btn_close.clicked.connect(self.accept)
        row.addWidget(btn_close)
        layout.addLayout(row)

        self.load_items()

    def load_items(self):
        self.tree.clear()
        for entry in list_disabled_mods(include_missing=True):
            disabled_path = entry["disabled_path"]
            item = QTreeWidgetItem([
                disabled_path,
                entry.get("label") or REASONS.get(entry.get("reason"), entry.get("reason", "")),
                entry.get("source_process", ""),
                entry.get("disabled_at", ""),
                t("Sim") if entry.get("exists") else t("Não"),
            ])
            item.setToolTip(0, disabled_path)
            item.setData(0, Qt.ItemDataRole.UserRole, disabled_path)
            self.tree.addTopLevelItem(item)

    def selected_disabled_paths(self):
        return [item.data(0, Qt.ItemDataRole.UserRole) for item in self.tree.selectedItems()]

    def on_restore(self):
        paths = self.selected_disabled_paths()
        if not paths:
            return
        restored = 0
        failed = []
        for path in paths:
            ok, msg = restore_disabled(path)
            if ok:
                restored += 1
            else:
                failed.append(f"{path}: {msg}")
        message = t("{} mod(s) restaurado(s).").format(restored)
        if failed:
            message += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Restauração"), message)
        self.load_items()

    def on_delete(self):
        paths = self.selected_disabled_paths()
        if not paths:
            return
        preview = "\n".join(paths[:10])
        if len(paths) > 10:
            preview += "\n" + t("... e mais {} item(s)").format(len(paths) - 10)
        reply = QMessageBox.question(
            self,
            t("Confirmar Exclusão"),
            t("Excluir permanentemente {} mod(s) desativado(s)?\n\n{}").format(len(paths), preview),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply != QMessageBox.StandardButton.Yes:
            return
        deleted = 0
        failed = []
        for path in paths:
            if delete_disabled(path):
                deleted += 1
            else:
                failed.append(path)
        message = t("{} mod(s) desativado(s) excluído(s).").format(deleted)
        if failed:
            message += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Exclusão"), message)
        self.load_items()

# ====================================================================
# ABA PRINCIPAL: ORGANIZADOR
# ====================================================================

class OrganizerTab(QWidget):
    def __init__(self):
        super().__init__()
        self.all_items_flat = [] # Lista plana para busca
        self.search_timer = QTimer(self)
        self.search_timer.setSingleShot(True)
        self.search_timer.timeout.connect(self.apply_search)
        self.pending_search_text = ""
        self.build_ui()

    def build_ui(self):
        layout = QVBoxLayout(self)
        layout.setSpacing(10)

        title = QLabel(t("📂 Organizador Global de Mods"))
        title.setObjectName("HeaderTitle")
        layout.addWidget(title)

        desc = QLabel(t("Explore, mova, copie, renomeie e limpe sua pasta Mods com operações seguras dentro do The Sims 4."))
        desc.setObjectName("SubDescription")
        layout.addWidget(desc)

        # Barra de Ações Rápidas (Tools)
        tools_layout = QHBoxLayout()
        
        btn_refresh = QPushButton(t("🔄 Recarregar Árvore"))
        btn_refresh.clicked.connect(self.load_tree)
        tools_layout.addWidget(btn_refresh)
        
        btn_junk = QPushButton(t("🧹 Limpeza de Lixo"))
        btn_junk.clicked.connect(self.on_junk_clean)
        tools_layout.addWidget(btn_junk)
        
        btn_scripts = QPushButton(t("⚠️ Checar Scripts"))
        btn_scripts.clicked.connect(self.on_check_scripts)
        tools_layout.addWidget(btn_scripts)
        
        btn_dup = QPushButton(t("🔍 Buscar Duplicatas"))
        btn_dup.setObjectName("danger")
        btn_dup.clicked.connect(self.on_find_duplicates)
        tools_layout.addWidget(btn_dup)

        btn_disabled = QPushButton(t("Ver Desativados"))
        btn_disabled.clicked.connect(self.on_show_disabled)
        tools_layout.addWidget(btn_disabled)
        
        layout.addLayout(tools_layout)

        # Barra de Pesquisa e Organização
        search_move_layout = QHBoxLayout()
        
        self.entry_search = QLineEdit()
        self.entry_search.setPlaceholderText(t("Buscar mod pelo nome..."))
        self.entry_search.textChanged.connect(self.on_search)
        search_move_layout.addWidget(self.entry_search, stretch=3)
        
        btn_new_folder = QPushButton(t("➕ Nova Pasta"))
        btn_new_folder.setObjectName("primary")
        btn_new_folder.clicked.connect(self.on_create_folder)
        search_move_layout.addWidget(btn_new_folder, stretch=1)

        btn_move = QPushButton(t("Mover Para..."))
        btn_move.clicked.connect(lambda: self.on_transfer_items(copy_mode=False))
        search_move_layout.addWidget(btn_move, stretch=1)

        btn_copy = QPushButton(t("Copiar Para..."))
        btn_copy.clicked.connect(lambda: self.on_transfer_items(copy_mode=True))
        search_move_layout.addWidget(btn_copy, stretch=1)

        btn_rename = QPushButton(t("Renomear"))
        btn_rename.clicked.connect(self.on_rename_item)
        search_move_layout.addWidget(btn_rename, stretch=1)

        btn_delete = QPushButton(t("Excluir"))
        btn_delete.setObjectName("danger")
        btn_delete.clicked.connect(self.on_delete_items)
        search_move_layout.addWidget(btn_delete, stretch=1)
        
        layout.addLayout(search_move_layout)

        # Tree Widget (Visualizador)
        self.tree = QTreeWidget()
        self.tree.setHeaderLabels([t("Nome do Arquivo/Pasta"), t("Tamanho")])
        self.tree.header().setMinimumHeight(34)
        self.tree.header().setDefaultAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        self.tree.header().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        self.tree.header().setSectionResizeMode(1, QHeaderView.ResizeMode.ResizeToContents)
        self.tree.setTextElideMode(Qt.TextElideMode.ElideNone)
        self.tree.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.tree.setIndentation(18)
        self.tree.setUniformRowHeights(True)
        self.tree.setStyleSheet("""
            QTreeWidget { padding: 8px 10px; }
            QHeaderView::section { min-height: 28px; padding: 7px 10px; }
            QTreeWidget::item { min-height: 34px; padding: 4px 8px; }
        """)
        self.tree.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.tree.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.tree.customContextMenuRequested.connect(self.open_context_menu)
        
        layout.addWidget(self.tree)

        self.load_tree()

    # --- CARREGAMENTO DA ÁRVORE ---
    def load_tree(self):
        mods_dir = get_mods_dir()
        if not mods_dir: return
        
        self.tree.clear()
        self.all_items_flat.clear()
        
        # Cria item de carregamento
        loading_item = QTreeWidgetItem([t("⏳ Carregando sua pasta Mods gigante..."), ""])
        self.tree.addTopLevelItem(loading_item)
        self.tree.setEnabled(False)
        
        self.tree_worker = TreeLoaderWorker(mods_dir)
        self.tree_worker.finished_signal.connect(self.on_tree_loaded)
        self.tree_worker.start()

    def populate_tree_recursive(self, parent_item, node_data):
        for child_data in node_data['children']:
            is_dir = child_data['type'] == 'dir'
            name = child_data['name']
            path = child_data['path']
            
            # Format size if file
            size_str = ""
            if not is_dir:
                size_bytes = child_data.get('size', 0)
                size_str = f"{size_bytes / (1024*1024):.2f} MB"
                if name.endswith('.disabled'):
                    name = f"🔴 {name}"
                elif name.endswith('.ts4script'):
                    name = f"📜 {name}"
                else:
                    name = f"📄 {name}"
            else:
                name = f"📁 {name}"

            item = QTreeWidgetItem([name, size_str])
            item.setData(0, Qt.ItemDataRole.UserRole, path)
            item.setData(1, Qt.ItemDataRole.UserRole, is_dir)
            
            parent_item.addChild(item)
            self.all_items_flat.append(item)
            
            if is_dir:
                self.populate_tree_recursive(item, child_data)

    def on_tree_loaded(self, tree_dict, errors):
        self.tree.clear()
        self.tree.setEnabled(True)
        if errors:
            QMessageBox.warning(self, t("Aviso"), t("Algumas pastas não puderam ser lidas:\n{}").format("\n".join(errors[:10])))
        
        if not tree_dict['children']:
            self.tree.addTopLevelItem(QTreeWidgetItem([t("Pasta Mods está vazia."), ""]))
            return
            
        root_item = self.tree.invisibleRootItem()
        self.populate_tree_recursive(root_item, tree_dict)

    # --- PESQUISA ---
    def on_search(self, text):
        self.pending_search_text = text
        self.search_timer.start(300)

    def apply_search(self):
        search_text = self.pending_search_text.lower()
        for item in self.all_items_flat:
            name = item.text(0).lower()
            if search_text in name:
                item.setHidden(False)
                # Ensure parent is expanded and visible
                parent = item.parent()
                while parent:
                    parent.setHidden(False)
                    parent.setExpanded(True)
                    parent = parent.parent()
            else:
                item.setHidden(True)
                
        # If search is cleared, collapse everything back to default
        if not search_text:
            for item in self.all_items_flat:
                item.setHidden(False)
                if item.data(1, Qt.ItemDataRole.UserRole): # se for pasta
                    item.setExpanded(False)

    # --- MENU DE CONTEXTO (Desativar Mod) ---
    def open_context_menu(self, position):
        selected_items = self.tree.selectedItems()
        if not selected_items: return
        
        menu = QMenu(self)
        action_new_folder = QAction(t("➕ Nova Pasta Aqui"), self)
        action_new_folder.triggered.connect(self.on_create_folder)
        menu.addAction(action_new_folder)

        action_move = QAction(t("Mover Para..."), self)
        action_move.triggered.connect(lambda: self.on_transfer_items(copy_mode=False))
        menu.addAction(action_move)

        action_copy = QAction(t("Copiar Para..."), self)
        action_copy.triggered.connect(lambda: self.on_transfer_items(copy_mode=True))
        menu.addAction(action_copy)

        action_rename = QAction(t("Renomear"), self)
        action_rename.triggered.connect(self.on_rename_item)
        menu.addAction(action_rename)

        action_toggle = QAction(t("🔴 Ligar / Desligar Mod (.disabled)"), self)
        action_toggle.triggered.connect(self.toggle_selected_mods)
        menu.addAction(action_toggle)

        action_delete = QAction(t("Excluir"), self)
        action_delete.triggered.connect(self.on_delete_items)
        menu.addAction(action_delete)
        
        menu.exec(self.tree.viewport().mapToGlobal(position))

    def toggle_selected_mods(self):
        selected_files = [
            item.data(0, Qt.ItemDataRole.UserRole)
            for item in self.tree.selectedItems()
            if item.data(0, Qt.ItemDataRole.UserRole) and not item.data(1, Qt.ItemDataRole.UserRole)
        ]
        if not selected_files:
            return

        disabling = [path for path in selected_files if not path.endswith(".disabled")]
        reason = "manual"
        note = ""
        if disabling:
            labels = [
                REASONS["suspected_bug"],
                REASONS["testing"],
                REASONS["outdated"],
                REASONS["duplicate"],
                REASONS["manual"],
            ]
            label_to_reason = {label: key for key, label in REASONS.items()}
            selected_label, ok = QInputDialog.getItem(
                self,
                t("Motivo da Desativação"),
                t("Por que deseja desativar o(s) mod(s)?"),
                labels,
                0,
                False,
            )
            if not ok:
                return
            reason = label_to_reason.get(selected_label, "manual")
            note, _ = QInputDialog.getText(self, t("Observação"), t("Observação opcional:"))

        failed = []
        changed = 0
        for path in selected_files:
            try:
                if path.endswith(".disabled"):
                    if path not in [entry["disabled_path"] for entry in list_disabled_mods(include_missing=True)]:
                        register_existing_disabled(path, reason="manual", source_process="organizer")
                    ok, msg = restore_disabled(path)
                    if not ok:
                        failed.append(f"{path}: {msg}")
                    else:
                        changed += 1
                else:
                    disable_mod(path, reason=reason, note=note, source_process="organizer")
                    changed += 1
            except Exception as e:
                failed.append(f"{path}: {e}")
        if changed:
            self.load_tree()
        if failed:
            QMessageBox.warning(self, t("Erro"), t("Alguns mods não puderam ser alterados:\n{}").format("\n".join(failed[:10])))

    def on_show_disabled(self):
        dialog = DisabledModsDialog(self)
        dialog.exec()
        self.load_tree()

    # --- EXPLORADOR SEGURO ---
    def get_selected_paths(self):
        paths = []
        for item in self.tree.selectedItems():
            path = item.data(0, Qt.ItemDataRole.UserRole)
            if path:
                paths.append(path)
        return paths

    def is_inside_mods_or_root(self, path):
        mods_dir = get_mods_dir()
        if not mods_dir or not path:
            return False
        try:
            return os.path.commonpath([os.path.realpath(path), os.path.realpath(mods_dir)]) == os.path.realpath(mods_dir)
        except ValueError:
            return False

    def is_mods_root(self, path):
        mods_dir = get_mods_dir()
        return bool(mods_dir and os.path.realpath(path) == os.path.realpath(mods_dir))

    def display_path(self, path):
        mods_dir = get_mods_dir()
        if mods_dir and self.is_inside_mods_or_root(path):
            rel = os.path.relpath(path, mods_dir)
            return "Mods" if rel == "." else os.path.join("Mods", rel)
        return path

    def validate_selected_paths(self, action_name, allow_root=False):
        paths = self.get_selected_paths()
        if not paths:
            QMessageBox.information(self, t("Aviso"), t("Selecione arquivos ou pastas na árvore para {}.").format(action_name))
            return []
        invalid = [p for p in paths if not self.is_inside_mods_or_root(p)]
        if invalid:
            QMessageBox.warning(self, t("Caminho inseguro"), t("A operação foi bloqueada porque há itens fora de Mods."))
            return []
        if not allow_root and any(self.is_mods_root(p) for p in paths):
            QMessageBox.warning(self, t("Caminho protegido"), t("A raiz da pasta Mods não pode ser usada nessa operação."))
            return []
        return paths

    def selected_folder_or_mods_root(self):
        mods_dir = get_mods_dir()
        selected = self.get_selected_paths()
        if len(selected) == 1 and os.path.isdir(selected[0]) and not self.is_mods_root(selected[0]):
            return selected[0]
        return mods_dir

    def on_create_folder(self):
        mods_dir = get_mods_dir()
        if not mods_dir:
            return
        parent_dir = self.selected_folder_or_mods_root()
        if not self.is_inside_mods_or_root(parent_dir):
            QMessageBox.warning(self, t("Caminho inseguro"), t("A nova pasta precisa ser criada dentro de Mods."))
            return

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
            QMessageBox.information(self, t("Pasta Criada"), t("Pasta criada em:\n{}").format(self.display_path(new_dir)))
            self.load_tree()
        except FileExistsError:
            QMessageBox.warning(self, t("Pasta já existe"), t("Já existe uma pasta com esse nome."))
        except Exception as e:
            QMessageBox.critical(self, t("Erro"), t("Falha ao criar pasta: {}").format(e))

    def choose_destination_folder(self, title):
        mods_dir = get_mods_dir()
        if not mods_dir:
            return None
        dialog = FolderPickerDialog(mods_dir, title, self)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            return dialog.selected_folder
        return None

    def on_transfer_items(self, copy_mode=False):
        action_name = t("copiar") if copy_mode else t("mover")
        paths = self.validate_selected_paths(action_name)
        if not paths:
            return

        dest_folder = self.choose_destination_folder(t("Copiar Para") if copy_mode else t("Mover Para"))
        if not dest_folder:
            return
        if not self.is_inside_mods_or_root(dest_folder):
            QMessageBox.warning(self, t("Caminho inseguro"), t("Destino bloqueado: escolha uma pasta dentro de Mods."))
            return

        done = 0
        failed = []
        script_warnings = []
        for src in paths:
            try:
                if os.path.isdir(src):
                    src_real = os.path.realpath(src)
                    dest_real = os.path.realpath(dest_folder)
                    if dest_real == src_real or dest_real.startswith(src_real + os.sep):
                        failed.append(t("{}: destino fica dentro da própria pasta.").format(self.display_path(src)))
                        continue

                dst = unique_dest_path(dest_folder, os.path.basename(src))
                if copy_mode:
                    if os.path.isdir(src):
                        shutil.copytree(src, dst)
                    else:
                        shutil.copy2(src, dst)
                else:
                    shutil.move(src, dst)
                done += 1
                self.collect_script_depth_warnings(dst, script_warnings)
            except Exception as e:
                failed.append(f"{self.display_path(src)}: {e}")

        msg = t("{} item(s) {} com sucesso.").format(done, t("copiado(s)") if copy_mode else t("movido(s)"))
        if script_warnings:
            msg += "\n\n" + t("Atenção: alguns scripts podem estar profundos demais para o The Sims 4:\n{}").format("\n".join(script_warnings[:8]))
        if failed:
            msg += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Operação Concluída"), msg)
        self.load_tree()

    def collect_script_depth_warnings(self, path, warnings):
        mods_dir = get_mods_dir()
        if not mods_dir:
            return
        paths = []
        if os.path.isfile(path):
            paths = [path]
        elif os.path.isdir(path):
            for root, _, files in os.walk(path):
                for f in files:
                    paths.append(os.path.join(root, f))
        for file_path in paths:
            if not file_path.lower().endswith(".ts4script"):
                continue
            rel_dir = os.path.relpath(os.path.dirname(file_path), mods_dir)
            depth = 0 if rel_dir == "." else len(rel_dir.split(os.sep))
            if depth > 1:
                warnings.append(self.display_path(file_path))

    def on_rename_item(self):
        paths = self.validate_selected_paths(t("renomear"))
        if not paths:
            return
        if len(paths) != 1:
            QMessageBox.information(self, t("Aviso"), t("Selecione apenas um item para renomear."))
            return

        src = paths[0]
        old_name = os.path.basename(src)
        new_name, ok = QInputDialog.getText(self, t("Renomear"), t("Novo nome:"), text=old_name)
        if not ok or not new_name.strip() or new_name == old_name:
            return
        safe_name = "".join(c for c in new_name.strip() if c not in '/\\:*?"<>|').strip()
        if not safe_name:
            QMessageBox.warning(self, t("Nome inválido"), t("Escolha um nome válido."))
            return
        dst = os.path.join(os.path.dirname(src), safe_name)
        if os.path.exists(dst):
            QMessageBox.warning(self, t("Já existe"), t("Já existe um item com esse nome na pasta atual."))
            return
        try:
            os.rename(src, dst)
            self.load_tree()
        except Exception as e:
            QMessageBox.critical(self, t("Erro"), t("Falha ao renomear: {}").format(e))

    def on_delete_items(self):
        paths = self.validate_selected_paths(t("excluir"))
        if not paths:
            return

        preview = "\n".join(self.display_path(p) for p in paths[:12])
        if len(paths) > 12:
            preview += "\n" + t("... e mais {} item(s)").format(len(paths) - 12)
        reply = QMessageBox.question(
            self,
            t("Confirmar Exclusão"),
            t("Excluir {} item(s)?\n\n{}").format(len(paths), preview),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply != QMessageBox.StandardButton.Yes:
            return

        deleted = 0
        failed = []
        for path in paths:
            if os.path.isdir(path):
                ok = safe_delete_folder(path)
            else:
                ok = safe_remove(path)
            if ok:
                deleted += 1
            else:
                failed.append(self.display_path(path))

        msg = t("{} item(s) excluído(s).").format(deleted)
        if failed:
            msg += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Exclusão Concluída"), msg)
        self.load_tree()

    # --- LIMPEZA DE LIXO ---
    def on_junk_clean(self):
        mods_dir = get_mods_dir()
        if not mods_dir: return
        
        reply = QMessageBox.question(self, t("Imagens de Referência"), 
            t("Deseja apagar também arquivos de IMAGEM (.jpg, .png)?\n\nMuitas imagens pesam o carregamento do jogo, mas criadores enviam fotos das roupas para você lembrar o que é. Quer apagar elas para máxima performance?"),
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No | QMessageBox.StandardButton.Cancel)
            
        if reply == QMessageBox.StandardButton.Cancel: return
        delete_images = (reply == QMessageBox.StandardButton.Yes)

        candidates = self.collect_junk_candidates(mods_dir, delete_images)
        if not candidates:
            QMessageBox.information(self, t("Limpeza"), t("Nenhum arquivo de lixo encontrado."))
            return

        dialog = QDialog(self)
        dialog.setWindowTitle(t("Revisar Limpeza"))
        dialog.resize(700, 500)
        layout = QVBoxLayout(dialog)
        lbl = QLabel(t("{} arquivo(s) serão removidos. Revise a lista antes de continuar.").format(len(candidates)))
        lbl.setWordWrap(True)
        layout.addWidget(lbl)
        list_widget = QListWidget()
        mods_real = os.path.realpath(mods_dir)
        for path in candidates[:500]:
            display = path
            if os.path.realpath(path).startswith(mods_real):
                display = os.path.relpath(path, mods_dir)
            list_widget.addItem(QListWidgetItem(display))
        if len(candidates) > 500:
            list_widget.addItem(QListWidgetItem(t("... e mais {} arquivo(s)").format(len(candidates) - 500)))
        layout.addWidget(list_widget)
        row = QHBoxLayout()
        btn_confirm = QPushButton(t("Excluir Listados"))
        btn_confirm.setObjectName("danger")
        btn_cancel = QPushButton(t("Cancelar"))
        row.addWidget(btn_confirm)
        row.addWidget(btn_cancel)
        layout.addLayout(row)
        btn_confirm.clicked.connect(dialog.accept)
        btn_cancel.clicked.connect(dialog.reject)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        
        self.clean_worker = JunkCleanerWorker(mods_dir, delete_images)
        self.clean_worker.finished_signal.connect(self.on_junk_cleaned)
        self.clean_worker.start()

    def collect_junk_candidates(self, mods_dir, delete_images):
        junk_exts = ['.txt', '.url', '.log', '.htm', '.html', '.rtf']
        if delete_images:
            junk_exts.extend(['.jpg', '.jpeg', '.png', '.bmp', '.gif'])

        candidates = []
        for root, _, files in os.walk(mods_dir):
            for f in files:
                ext = os.path.splitext(f)[1].lower()
                if ext in junk_exts or f.lower() in ['thumbs.db', '.ds_store']:
                    candidates.append(os.path.join(root, f))
        return candidates
        
    def on_junk_cleaned(self, f_del, d_del, failed):
        msg = t("Foram excluídos:\n\n- {} Arquivos Inúteis\n- {} Pastas Vazias").format(f_del, d_del)
        if failed:
            msg += "\n\n" + t("{} falha(s):\n{}").format(len(failed), "\n".join(failed[:10]))
        QMessageBox.information(self, t("Limpeza Concluída"), msg)
        self.load_tree()

    # --- VERIFICADOR DE SCRIPTS ---
    def on_check_scripts(self):
        mods_dir = get_mods_dir()
        if not mods_dir: return
        
        self.script_worker = ScriptCheckerWorker(mods_dir)
        self.script_worker.finished_signal.connect(self.on_scripts_checked)
        self.script_worker.start()
        
    def on_scripts_checked(self, bad_scripts):
        if not bad_scripts:
            QMessageBox.information(self, t("Verificação Concluída"), t("Tudo perfeito! Nenhum arquivo de Script está escondido fundo demais."))
        else:
            dialog = BadScriptsDialog(bad_scripts, self)
            if dialog.exec() == QDialog.DialogCode.Accepted:
                self.load_tree()

# --- DUPLICATAS ---
    def on_find_duplicates(self):
        mods_dir = get_mods_dir()
        if not mods_dir: return
            
        btn_sender = self.sender()
        if btn_sender:
            btn_sender.setEnabled(False)
            btn_sender.setText(t("⏳ Buscando..."))
            
        self.dup_worker = DuplicateWorker(mods_dir)
        self.dup_worker.finished_signal.connect(lambda dup_dict, errors, btn=btn_sender: self.on_duplicates_found(dup_dict, errors, btn))
        self.dup_worker.start()
        
    def on_duplicates_found(self, dup_dict, errors, btn):
        if btn:
            btn.setEnabled(True)
            btn.setText(t("🔍 Buscar Duplicatas"))
        if errors:
            QMessageBox.warning(self, t("Aviso"), t("Alguns arquivos não puderam ser analisados:\n{}").format("\n".join(errors[:10])))
            
        if not dup_dict:
            QMessageBox.information(self, t("Detector"), t("Nenhuma duplicata exata encontrada na sua pasta Mods!"))
            return
            
        dialog = DuplicatesDialog(dup_dict, self)
        if dialog.exec() == QDialog.DialogCode.Accepted:
            self.load_tree()
