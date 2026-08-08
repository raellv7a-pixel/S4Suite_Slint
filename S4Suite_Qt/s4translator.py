import os
import sys
import json
from s4common import ConfigManager

def resource_path(relative_path):
    try:
        base_path = sys._MEIPASS
    except Exception:
        base_path = os.path.dirname(__file__)
    return os.path.join(base_path, relative_path)

class Translator:
    _instance = None
    
    def __init__(self):
        self.current_lang = ConfigManager.get("language", "Português")
        self.translations = {}
        self.load_translations()
        
    def load_translations(self):
        if self.current_lang == "Português":
            return # Default language is Portuguese
            
        lang_file = "en.json" if self.current_lang == "English" else "es.json"
        locales_path = resource_path(os.path.join("locales", lang_file))
        
        if os.path.exists(locales_path):
            try:
                with open(locales_path, 'r', encoding='utf-8') as f:
                    self.translations = json.load(f)
            except Exception as e:
                print(f"Error loading translation file {locales_path}: {e}")
                self.translations = {}
        else:
            print(f"Translation file not found: {locales_path}")

    @classmethod
    def get_instance(cls):
        if cls._instance is None:
            cls._instance = Translator()
        return cls._instance

def t(text):
    translator = Translator.get_instance()
    if not text:
        return text
    return translator.translations.get(text, text)
