import json
import os
from deep_translator import GoogleTranslator

def translate_file(path, target_lang):
    print(f"Translating {path} to {target_lang}...")
    with open(path, 'r', encoding='utf-8') as f:
        data = json.load(f)
        
    translator = GoogleTranslator(source='pt', target=target_lang)
    keys = list(data.keys())
    
    # Process in batches to avoid overwhelming the free API
    for i in range(0, len(keys), 50):
        batch = keys[i:i+50]
        try:
            translations = translator.translate_batch(batch)
            for j, key in enumerate(batch):
                data[key] = translations[j]
        except Exception as e:
            print(f"Error on batch {i}: {e}")
            
    with open(path, 'w', encoding='utf-8') as f:
        json.dump(data, f, indent=4, ensure_ascii=False)
    print(f"Finished {path}")

base_dir = "/home/raell/Projetos/S4Suite_Qt/S4Suite_Qt"
translate_file(os.path.join(base_dir, "locales", "en.json"), "en")
translate_file(os.path.join(base_dir, "locales", "es.json"), "es")
