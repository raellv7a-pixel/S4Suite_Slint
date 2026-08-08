#!/usr/bin/env python3
"""Legacy placeholder.

The active S4 Suite UI is PyQt6 (`s4suite_qt.py`). The previous GTK entry point
mixed GTK containers with PyQt6 tabs and was not runnable. Keep this file only
to avoid ambiguous imports from old launchers.
"""

import sys

if __name__ == "__main__":
    print("s4suite_gtk.py is deprecated. Use s4suite_qt.py instead.")
    sys.exit(1)
