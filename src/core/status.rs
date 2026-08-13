//! Severidade das mensagens de status.
//!
//! O adapter escreve status como `"✅ Instalação concluída!"` em 77 pontos diferentes. Esse
//! marcador é útil no código-fonte — dá para ler a gravidade da mensagem de relance — mas não
//! deve chegar à tela: metade dos emojis vira `□` no renderizador do Slint, e nenhum deles
//! acompanha a cor do tema.
//!
//! Este módulo separa marcador de texto. A UI consulta pelos callbacks puros do global
//! `StatusRules` e desenha um ícone Tabler tingido pelo tema.

/// Gravidade de uma mensagem, na ordem em que a UI a colore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Idle,
    Busy,
    Ok,
    Warn,
    Error,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Idle => "idle",
            Kind::Busy => "busy",
            Kind::Ok => "ok",
            Kind::Warn => "warn",
            Kind::Error => "error",
        }
    }
}

/// Marcadores em uso no adapter, do mais específico ao mais genérico.
const MARCADORES: &[(char, Kind)] = &[
    ('✅', Kind::Ok),
    ('🎉', Kind::Ok),
    ('✨', Kind::Ok),
    ('⚠', Kind::Warn),
    ('❌', Kind::Error),
    ('🚫', Kind::Error),
    ('⏳', Kind::Busy),
    ('⚙', Kind::Busy),
    ('⚡', Kind::Busy),
    ('🔄', Kind::Busy),
    ('🧹', Kind::Busy),
    ('📌', Kind::Idle),
    ('ℹ', Kind::Idle),
    ('🛡', Kind::Ok),
    ('📥', Kind::Busy),
    ('🔍', Kind::Busy),
    ('🔎', Kind::Busy),
];

/// Separa o marcador do texto. Sem marcador, a mensagem é neutra e volta inteira.
fn separar(msg: &str) -> (Kind, &str) {
    let aparado = msg.trim_start();
    let Some(primeiro) = aparado.chars().next() else {
        return (Kind::Idle, msg);
    };
    let Some((_, kind)) = MARCADORES.iter().find(|(c, _)| *c == primeiro) else {
        return (Kind::Idle, msg);
    };

    // O seletor de variação (U+FE0F) e o espaço que segue o emoji fazem parte do marcador; deixá-
    // los para trás renderizaria um glifo invisível e um recuo torto no começo da linha.
    let resto = aparado[primeiro.len_utf8()..].trim_start_matches('\u{fe0f}').trim_start();
    (*kind, resto)
}

pub fn kind_of(msg: &str) -> Kind {
    separar(msg).0
}

pub fn label_of(msg: &str) -> &str {
    separar(msg).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marcador_vira_severidade_e_some_do_texto() {
        assert_eq!(kind_of("✅ Instalação concluída!"), Kind::Ok);
        assert_eq!(label_of("✅ Instalação concluída!"), "Instalação concluída!");

        assert_eq!(kind_of("⚠️ Configure a pasta do jogo antes de instalar mods."), Kind::Warn);
        assert_eq!(
            label_of("⚠️ Configure a pasta do jogo antes de instalar mods."),
            "Configure a pasta do jogo antes de instalar mods."
        );

        assert_eq!(kind_of("❌ Falha na instalação"), Kind::Error);
        assert_eq!(kind_of("⏳ Buscando..."), Kind::Busy);
    }

    #[test]
    fn mensagem_sem_marcador_fica_intacta() {
        assert_eq!(kind_of("Pronto"), Kind::Idle);
        assert_eq!(label_of("Pronto"), "Pronto");
        assert_eq!(label_of(""), "");
        assert_eq!(kind_of(""), Kind::Idle);
    }

    #[test]
    fn emoji_que_nao_e_marcador_nao_e_engolido() {
        // Um nome de mod que comece com emoji não pode perder o primeiro caractere.
        assert_eq!(label_of("🐧 Ambiente detectado"), "🐧 Ambiente detectado");
        assert_eq!(kind_of("🐧 Ambiente detectado"), Kind::Idle);
    }

    #[test]
    fn o_seletor_de_variacao_vai_junto_com_o_marcador() {
        // "⚠\u{fe0f} texto" e "⚠ texto" precisam produzir exatamente o mesmo rótulo.
        assert_eq!(label_of("⚠\u{fe0f} Cuidado"), "Cuidado");
        assert_eq!(label_of("⚠ Cuidado"), "Cuidado");
    }
}
