import sys
from PyQt6.QtWidgets import QApplication
from s4suite_qt import S4SuiteMainWindow

try:
    app = QApplication(sys.argv)
    window = S4SuiteMainWindow()
    print("SUCCESS: App initialized with language:", window.config_tab.lang_combo.currentText())
    sys.exit(0)
except Exception as e:
    import traceback
    traceback.print_exc()
    sys.exit(1)
