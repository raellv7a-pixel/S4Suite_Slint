//! Garante que toda chave usada na UI existe nos locales e que o mecanismo de
//! tradução responde. Sem isto, uma string nova na UI passaria despercebida e
//! apareceria em português no meio de uma interface em inglês.

use s4suite::core::i18n::t;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Extrai as chaves `I18n.tr("...")` de todos os arquivos .slint.
fn ui_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")];

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().map(|e| e != "slint").unwrap_or(true) {
                continue;
            }
            let src = fs::read_to_string(&path).unwrap();
            let mut rest = src.as_str();
            while let Some(start) = rest.find("I18n.tr(\"") {
                rest = &rest[start + 9..];
                if let Some(end) = rest.find("\")") {
                    keys.insert(rest[..end].to_string());
                    rest = &rest[end..];
                }
            }
        }
    }
    keys
}

fn locale(lang: &str) -> serde_json::Map<String, serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/locales")
        .join(format!("{}.json", lang));
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn ui_tem_chaves_traduziveis() {
    let keys = ui_keys();
    assert!(keys.len() > 50, "só {} chaves extraídas — o parser quebrou?", keys.len());
}

#[test]
fn toda_chave_da_ui_existe_em_ingles_e_espanhol() {
    let keys = ui_keys();

    for lang in ["en", "es"] {
        let dict = locale(lang);
        let faltando: Vec<&String> = keys.iter().filter(|k| !dict.contains_key(*k)).collect();
        assert!(
            faltando.is_empty(),
            "{}.json sem tradução para {} chave(s): {:?}",
            lang,
            faltando.len(),
            faltando
        );
    }
}

#[test]
fn nenhuma_traducao_esta_vazia() {
    for lang in ["en", "es", "pt"] {
        for (key, value) in locale(lang) {
            let texto = value.as_str().unwrap_or("");
            assert!(!texto.trim().is_empty(), "{}.json: chave {:?} sem texto", lang, key);
        }
    }
}

#[test]
fn portugues_e_o_idioma_base_e_devolve_a_propria_chave() {
    assert_eq!(t("Salvar", "pt"), "Salvar");
    assert_eq!(
        t("Uma frase que ninguém traduziu", "pt"),
        "Uma frase que ninguém traduziu"
    );
}

#[test]
fn traducao_responde_nos_tres_idiomas() {
    assert_eq!(t("Salvar", "en"), "Save");
    assert_eq!(t("Salvar", "es"), "Guardar");
    assert_eq!(t("Configurações", "en"), "Settings");
}

/// Chave desconhecida nunca deve virar string vazia: o rótulo some da tela.
#[test]
fn chave_desconhecida_volta_como_esta() {
    let inventada = "Texto que não existe em locale nenhum";
    for lang in ["en", "es", "pt", "de"] {
        assert_eq!(t(inventada, lang), inventada);
    }
}

#[test]
fn avisos_de_seguranca_mantem_severidade_visivel_nos_tres_idiomas() {
    use s4suite::core::status::{kind_of, label_of};
    for key in [
        "⚠️ Configure a pasta do jogo primeiro.",
        "⚠️ Outra operação de manutenção está em andamento.",
    ] {
        for lang in ["pt", "en", "es"] {
            let translated = t(key, lang);
            assert_eq!(kind_of(&translated).as_str(), "warn");
            assert!(!label_of(&translated).starts_with('⚠'));
        }
    }
}
