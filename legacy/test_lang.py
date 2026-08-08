import sys
from PyQt6.QtWidgets import QApplication, QLabel
from s4translator import t

app = QApplication(sys.argv)
label = QLabel(t("🏠 Início"))
print("Translated text is:", label.text())
