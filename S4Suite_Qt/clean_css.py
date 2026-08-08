import os
import glob
import re

for filepath in glob.glob('/home/raell/Projetos/S4Suite_Qt/S4Suite_Qt/ui/*.py'):
    with open(filepath, 'r') as f:
        content = f.read()

    content = re.sub(r'self\.list_widget\.setStyleSheet\("""\s*QListWidget.*?"""\)', '', content, flags=re.DOTALL)
    content = re.sub(r'self\.log_console\.setStyleSheet\(".*?"\)', '', content)
    content = re.sub(r'([a-zA-Z0-9_\.]+)\.setStyleSheet\("background-color: #2a82da.*?"\)', r'\1.setObjectName("primary")', content)
    content = re.sub(r'([a-zA-Z0-9_\.]+)\.setStyleSheet\("background-color: #[cd]9[47][07][43][06].*?"\)', r'\1.setObjectName("danger")', content)
    
    content = re.sub(r'color:\s*#69d3ff;?\s*', '', content)
    content = re.sub(r'color:\s*#[ac][05][ac][05][ac][05];?\s*', '', content)
    content = re.sub(r'color:\s*#a0a0a0;?\s*', '', content)

    with open(filepath, 'w') as f:
        f.write(content)

print("CSS legacy removed from UI files.")
