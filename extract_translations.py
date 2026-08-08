import os
import re
import json

base_dir = "/home/raell/Projetos/S4Suite_Qt/S4Suite_Qt"
# More precise regex to only match t("...") or t('...')
pattern = re.compile(r"t\(\s*\"(.*?)\"\s*\)|t\(\s*'(.*?)'\s*\)")

strings = set()

for root, _, files in os.walk(base_dir):
    if "venv" in root or "__pycache__" in root or ".git" in root or "build" in root:
        continue
    for f in files:
        if f.endswith(".py"):
            path = os.path.join(root, f)
            with open(path, "r", encoding="utf-8") as file:
                content = file.read()
                matches = pattern.findall(content)
                for m in matches:
                    s = m[0] if m[0] else m[1]
                    if s:
                        strings.add(s)

result = {s: s for s in sorted(list(strings))}
with open(os.path.join(base_dir, "locales", "en.json"), "w", encoding="utf-8") as f:
    json.dump(result, f, indent=4, ensure_ascii=False)

with open(os.path.join(base_dir, "locales", "es.json"), "w", encoding="utf-8") as f:
    json.dump(result, f, indent=4, ensure_ascii=False)

print(f"Extracted {len(strings)} strings")
