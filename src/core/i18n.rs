use parking_lot::RwLock;
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(RustEmbed)]
#[folder = "assets/locales/"]
struct LocaleAssets;

pub struct Translator {
    translations: RwLock<HashMap<String, HashMap<String, String>>>,
}

static TRANSLATOR: LazyLock<Translator> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for lang in &["en", "es", "pt"] {
        let file_name = format!("{}.json", lang);
        if let Some(file) = LocaleAssets::get(&file_name) {
            if let Ok(json_map) = serde_json::from_slice::<HashMap<String, Option<String>>>(&file.data) {
                let clean_map: HashMap<String, String> = json_map
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|val| (k, val)))
                    .collect();
                map.insert(lang.to_string(), clean_map);
            }
        }
    }
    Translator {
        translations: RwLock::new(map),
    }
});

impl Translator {
    pub fn global() -> &'static Translator {
        &TRANSLATOR
    }

    pub fn t(&self, key: &str, lang: &str) -> String {
        let lock = self.translations.read();
        if let Some(dict) = lock.get(lang) {
            if let Some(val) = dict.get(key) {
                return val.clone();
            }
        }
        key.to_string()
    }
}

pub fn t(key: &str, lang: &str) -> String {
    Translator::global().t(key, lang)
}
