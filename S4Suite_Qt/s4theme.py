class ThemeManager:
    THEMES = {
        "Sims Green": {
            "bg": "#0B110D", 
            "panel": "#121A14", 
            "accent1": "#00A86B", 
            "accent2": "#00FF7F", 
            "text": "#E0E6ED", 
            "border": "#1A2E22"
        },
        "Deep Blue": {
            "bg": "#0B0F19", 
            "panel": "#111622", 
            "accent1": "#007BFF", 
            "accent2": "#00E5FF", 
            "text": "#E0E6ED", 
            "border": "#1E253A"
        },
        "Cyber Purple": {
            "bg": "#0E0A14", 
            "panel": "#14101A", 
            "accent1": "#8A2BE2", 
            "accent2": "#DDA0DD", 
            "text": "#E0E6ED", 
            "border": "#281D33"
        },
        "Neon Pink": {
            "bg": "#120A0F", 
            "panel": "#1A1016", 
            "accent1": "#FF1493", 
            "accent2": "#FF69B4", 
            "text": "#E0E6ED", 
            "border": "#331C28"
        },
    }
    
    @staticmethod
    def get_stylesheet(theme_name="Sims Green"):
        colors = ThemeManager.THEMES.get(theme_name, ThemeManager.THEMES["Sims Green"])
        def rgba(hex_color, alpha):
            clean = hex_color.lstrip("#")
            r, g, b = int(clean[0:2], 16), int(clean[2:4], 16), int(clean[4:6], 16)
            return f"rgba({r}, {g}, {b}, {int(alpha * 255)})"

        selected_soft = rgba(colors["accent1"], 0.28)
        return f"""
        QWidget {{
            font-family: 'Inter', 'Segoe UI', 'Roboto', sans-serif;
            color: {colors['text']};
        }}
        QMainWindow {{
            background-color: {colors['bg']};
        }}
        /* Sidebar container */
        QFrame#Sidebar {{
            background-color: {colors['panel']};
            border-right: 1px solid {colors['border']};
        }}
        /* Sidebar buttons */
        QPushButton[class="SidebarBtn"] {{
            background: transparent;
            color: #8B9BB4;
            text-align: left;
            padding: 15px 20px;
            font-size: 15px;
            font-weight: bold;
            border: none;
            border-radius: 8px;
            margin: 2px 10px;
        }}
        QPushButton[class="SidebarBtn"]:hover {{
            background: rgba(255, 255, 255, 0.05);
            color: #ffffff;
        }}
        QPushButton[class="SidebarBtn"][active="true"] {{
            background: qlineargradient(x1:0, y1:0, x2:1, y2:0, stop:0 {colors['accent1']}, stop:1 {colors['accent2']});
            color: white;
        }}
        /* Cards */
        QFrame[class="StatCard"] {{
            background-color: {colors['panel']};
            border: 1px solid {colors['border']};
            border-radius: 10px;
        }}
        /* GroupBox */
        QGroupBox {{
            border: 1px solid {colors['border']};
            border-radius: 8px;
            margin-top: 1.5em;
            padding-top: 1em;
            background-color: {colors['panel']};
        }}
        QGroupBox::title {{
            subcontrol-origin: margin;
            subcontrol-position: top left;
            left: 15px;
            padding: 0 5px;
            color: {colors['accent2']};
            font-weight: bold;
        }}
        /* Regular Buttons */
        QPushButton {{
            background-color: {colors['panel']};
            color: {colors['text']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 8px 16px;
            font-weight: bold;
        }}
        QPushButton:hover {{
            border-color: {colors['accent2']};
            background-color: {colors['border']};
        }}
        /* Primary Buttons */
        QPushButton#primary {{
            background: qlineargradient(x1:0, y1:0, x2:1, y2:0, stop:0 {colors['accent1']}, stop:1 {colors['accent2']});
            border: none;
            color: white;
        }}
        QPushButton#primary:hover {{
            background: qlineargradient(x1:0, y1:0, x2:1, y2:0, stop:0 {colors['accent2']}, stop:1 {colors['accent1']});
        }}
        /* Danger Buttons */
        QPushButton#danger {{
            background-color: #c94040;
            border: none;
            color: white;
        }}
        QPushButton#danger:hover {{
            background-color: #e54c4c;
        }}
        /* Inputs & Lists */
        QLineEdit, QComboBox, QListWidget, QTextEdit, QTreeWidget, QScrollArea, QFileDialog, QAbstractItemView, QTableView, QListView, QTreeView {{
            background-color: {colors['bg']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 8px;
            color: {colors['text']};
        }}
        QComboBox {{
            min-height: 18px;
        }}
        QComboBox QAbstractItemView {{
            background-color: {colors['bg']};
            color: {colors['text']};
            selection-background-color: {colors['accent1']};
            selection-color: white;
            border: 1px solid {colors['accent2']};
            outline: 0;
            padding: 6px;
        }}
        QComboBox QAbstractItemView::item {{
            min-height: 30px;
            padding: 6px 10px;
            border: none;
        }}
        QMenu {{
            background-color: {colors['panel']};
            color: {colors['text']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 6px;
        }}
        QMenu::item {{
            background: transparent;
            color: {colors['text']};
            padding: 8px 26px 8px 12px;
            border-radius: 4px;
            min-width: 180px;
        }}
        QMenu::item:selected {{
            background-color: {colors['accent1']};
            color: white;
        }}
        QMenu::separator {{
            height: 1px;
            background-color: {colors['border']};
            margin: 5px 8px;
        }}
        QLineEdit:focus, QListWidget:focus, QTreeWidget:focus, QTableView:focus, QListView:focus, QTreeView:focus {{
            border: 1px solid {colors['accent2']};
        }}
        QListWidget::item, QTreeWidget::item, QTableView::item, QListView::item, QTreeView::item {{
            border-bottom: 1px solid {colors['border']};
            min-height: 45px;
            color: {colors['text']};
        }}
        QListWidget::item:hover, QTreeWidget::item:hover, QTableView::item:hover, QListView::item:hover, QTreeView::item:hover {{
            background-color: {colors['border']};
        }}
        QListWidget::item:selected, QTreeWidget::item:selected, QTableView::item:selected, QListView::item:selected, QTreeView::item:selected {{
            background-color: {colors['accent1']};
            color: white;
        }}
        QRadioButton, QCheckBox, QLabel {{
            color: {colors['text']};
            background: transparent;
        }}
        QRadioButton::indicator, QCheckBox::indicator {{
            width: 14px;
            height: 14px;
        }}
        QScrollArea > QWidget > QWidget {{
            background-color: {colors['bg']};
            color: {colors['text']};
        }}
        QFileDialog QWidget {{
            background-color: {colors['bg']};
            color: {colors['text']};
        }}
        QFileDialog QHeaderView::section {{
            background-color: {colors['panel']};
            color: {colors['accent2']};
        }}
        QHeaderView::section {{
            background-color: {colors['panel']};
            color: {colors['accent2']};
            min-height: 30px;
            padding: 7px 10px;
            border: 1px solid {colors['border']};
            font-weight: bold;
        }}
        QTreeWidget#MergeJobsTree, QTreeWidget#DisabledModsTree {{
            background-color: {colors['bg']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 0px;
            outline: 0;
        }}
        QTreeWidget#MergeJobsTree:focus, QTreeWidget#DisabledModsTree:focus {{
            border: 1px solid {colors['border']};
        }}
        QTreeWidget#MergeJobsTree QHeaderView::section, QTreeWidget#DisabledModsTree QHeaderView::section {{
            background-color: {colors['panel']};
            color: {colors['accent2']};
            border: none;
            border-right: 1px solid {colors['border']};
            border-bottom: 1px solid {colors['border']};
            min-height: 34px;
            padding: 8px 14px;
        }}
        QTreeWidget#MergeJobsTree::item, QTreeWidget#DisabledModsTree::item {{
            min-height: 36px;
            padding: 6px 14px;
            border-right: 1px solid {colors['border']};
            border-bottom: 1px solid {colors['border']};
        }}
        QTreeWidget#MergeJobsTree::item:selected, QTreeWidget#DisabledModsTree::item:selected {{
            background-color: {selected_soft};
            color: {colors['text']};
        }}
        QListWidget#MergeRiskList {{
            background-color: {colors['bg']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 6px;
            outline: 0;
        }}
        QListWidget#MergeRiskList::item {{
            min-height: 32px;
            padding: 6px 8px;
            border-bottom: 1px solid {colors['border']};
        }}
        QInputDialog, QMessageBox {{
            background-color: {colors['bg']};
            color: {colors['text']};
        }}
        QInputDialog QLabel, QMessageBox QLabel {{
            color: {colors['text']};
            background: transparent;
        }}
        QInputDialog QLineEdit, QInputDialog QComboBox {{
            background-color: {colors['panel']};
            color: {colors['text']};
            border: 1px solid {colors['border']};
            border-radius: 6px;
            padding: 8px;
        }}
        QInputDialog QListView, QInputDialog QComboBox QAbstractItemView {{
            background-color: {colors['bg']};
            color: {colors['text']};
            selection-background-color: {colors['accent1']};
            selection-color: white;
            border: 1px solid {colors['accent2']};
        }}
        QMessageBox QPushButton, QInputDialog QPushButton {{
            min-width: 90px;
        }}
        /* Progress Bar */
        QProgressBar {{
            text-align: center;
            color: white;
            border: 1px solid {colors['border']};
            border-radius: 4px;
            background-color: {colors['panel']};
        }}
        QProgressBar::chunk {{
            background-color: {colors['accent2']};
            border-radius: 3px;
        }}
        /* Scrollbars */
        QScrollBar:vertical {{
            border: none;
            background: {colors['bg']};
            width: 8px;
            margin: 0px;
        }}
        QScrollBar::handle:vertical {{
            background: {colors['border']};
            border-radius: 4px;
            min-height: 20px;
        }}
        QScrollBar::handle:vertical:hover {{
            background: {colors['accent2']};
        }}
        QDialog {{
            background-color: {colors['bg']};
        }}
        """
