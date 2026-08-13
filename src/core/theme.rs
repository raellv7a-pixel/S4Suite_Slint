//! Resolução do tema visual do app.
//!
//! Existe um único tipo (`Roles`) e uma única função de aplicação (`apply`), usados tanto pelos
//! temas embutidos quanto pelas cores vindas da Hydra Shell. Manter os dois caminhos idênticos é
//! deliberado: um caminho separado para o tema do sistema seria a origem garantida de "essa cor
//! só está errada quando o tema vem da shell".
//!
//! Os papéis vivem aqui, no Rust, e não como expressões no `.slint`, porque em Slint uma
//! propriedade perde sua binding declarativa em definitivo assim que o Rust escreve nela. Um token
//! escrito como `active_theme == 0 ? #a : #b` pararia de reagir ao seletor de tema para sempre
//! depois da primeira aplicação de um tema do sistema.

use serde_json::Value;
use slint::Color;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Índice dos temas embutidos, na mesma ordem dos chips de Configurações.
pub const BUILTIN_NAMES: [&str; 4] = ["Sims Green", "Deep Blue", "Cyber Purple", "Neon Pink"];

/// Nome gravado em `config.theme` quando o usuário escolhe seguir a shell.
pub const SYSTEM_NAME: &str = "System";

/// Índice do chip "Sistema", logo depois dos 4 embutidos.
pub const SYSTEM_INDEX: i32 = BUILTIN_NAMES.len() as i32;

/// Intervalo de sondagem dos arquivos de tema da shell.
///
/// É polling de `mtime` em vez de um watcher de eventos (crate `notify`) de propósito: o app é
/// distribuído como AppImage e já usa threads para todo trabalho de fundo, então uma dependência
/// a menos vale mais do que a latência economizada. 700ms é imperceptível para uma troca de tema.
const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Conjunto completo de papéis Material Design 3.
///
/// Todo campo é obrigatório de propósito: `apply` escreve os 28 de uma vez. Aplicar apenas o
/// subconjunto que uma fonte trouxe deixa resíduo do tema anterior — falha que só se manifesta ao
/// alternar entre esquemas duas vezes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roles {
    pub primary: Color,
    pub on_primary: Color,
    pub primary_container: Color,
    pub on_primary_container: Color,

    pub secondary: Color,
    pub on_secondary: Color,
    pub tertiary: Color,
    pub on_tertiary: Color,

    pub surface_dim: Color,
    pub surface: Color,
    pub surface_bright: Color,
    pub surface_container_lowest: Color,
    pub surface_container_low: Color,
    pub surface_container: Color,
    pub surface_container_high: Color,
    pub surface_container_highest: Color,

    pub on_surface: Color,
    pub on_surface_variant: Color,
    pub outline: Color,
    pub outline_variant: Color,
    pub scrim: Color,
    pub shadow: Color,

    pub error: Color,
    pub on_error: Color,
    pub error_container: Color,
    pub on_error_container: Color,
    pub success_container: Color,
    pub on_success_container: Color,

    pub warning: Color,
    pub info: Color,

    pub is_dark: bool,
    pub from_system: bool,
}

const fn rgb(hex: u32) -> Color {
    Color::from_argb_encoded(0xff00_0000 | hex)
}

/// Rampa neutra escura compartilhada pelos temas embutidos, do mais fundo ao mais alto.
/// Ordem: dim, surface, container_lowest, container_low, container, container_high,
/// container_highest, bright.
const NEUTRAL_DARK: [u32; 8] = [
    0x0b1020, 0x0f172a, 0x0a0f1c, 0x141c30, 0x1e293b, 0x334155, 0x475569, 0x2a3550,
];

/// Cada tema embutido: primary, on_primary, primary_container, on_primary_container,
/// secondary, on_secondary, tertiary, on_tertiary.
const BUILTIN_ACCENTS: [[u32; 8]; 4] = [
    // Sims Green
    [0x4ade80, 0x003917, 0x15803d, 0xb9f6ca, 0xb8ccbb, 0x243528, 0x34d399, 0x00382d],
    // Deep Blue
    [0x93b4ff, 0x002a78, 0x1e40af, 0xd9e2ff, 0xbfc6dc, 0x293042, 0x38bdf8, 0x003546],
    // Cyber Purple
    [0xd8b4fe, 0x3b0764, 0x7e22ce, 0xf3e8ff, 0xcbc2db, 0x332d41, 0xc084fc, 0x3b0764],
    // Neon Pink
    [0xffb1c8, 0x5b1133, 0xbe185d, 0xffd9e2, 0xe0bdc8, 0x422833, 0xfda4af, 0x5c1122],
];

/// Mistura `base` com `tint` na proporção `amount` (0.0 = só base, 1.0 = só tint).
fn mix(base: Color, tint: Color, amount: f32) -> Color {
    let f = amount.clamp(0.0, 1.0);
    let blend = |a: u8, b: u8| ((a as f32) * (1.0 - f) + (b as f32) * f).round() as u8;
    Color::from_rgb_u8(
        blend(base.red(), tint.red()),
        blend(base.green(), tint.green()),
        blend(base.blue(), tint.blue()),
    )
}

/// Luminância relativa perceptual, usada para decidir claro/escuro quando a fonte não diz.
pub fn is_dark_color(c: Color) -> bool {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.red()) + 0.7152 * lin(c.green()) + 0.0722 * lin(c.blue()) < 0.18
}

impl Roles {
    /// Um dos 4 temas do app. Índice fora da faixa cai no primeiro.
    pub fn builtin(index: i32) -> Self {
        // Fora da faixa cai no primeiro tema, e não no último — precisa casar com
        // `builtin_name_from_index`, senão o nome gravado no config diverge da cor aplicada.
        let accents = usize::try_from(index)
            .ok()
            .and_then(|i| BUILTIN_ACCENTS.get(i))
            .copied()
            .unwrap_or(BUILTIN_ACCENTS[0]);
        let primary = rgb(accents[0]);

        // As superfícies recebem um sopro do primary (6%) — é o "surface tint" do MD3, o que faz
        // um tema roxo não ter exatamente o mesmo cinza de um tema verde. Acima disso a interface
        // começa a parecer colorida em vez de neutra.
        let surf = |i: usize| mix(rgb(NEUTRAL_DARK[i]), primary, 0.06);

        Self {
            primary,
            on_primary: rgb(accents[1]),
            primary_container: rgb(accents[2]),
            on_primary_container: rgb(accents[3]),

            secondary: rgb(accents[4]),
            on_secondary: rgb(accents[5]),
            tertiary: rgb(accents[6]),
            on_tertiary: rgb(accents[7]),

            surface_dim: surf(0),
            surface: surf(1),
            surface_container_lowest: surf(2),
            surface_container_low: surf(3),
            surface_container: surf(4),
            surface_container_high: surf(5),
            surface_container_highest: surf(6),
            surface_bright: surf(7),

            on_surface: rgb(0xf8fafc),
            on_surface_variant: rgb(0x94a3b8),
            outline: rgb(0x64748b),
            outline_variant: mix(rgb(0x334155), primary, 0.06),
            scrim: rgb(0x000000),
            shadow: rgb(0x000000),

            error: rgb(0xef4444),
            on_error: rgb(0x450a0a),
            error_container: rgb(0x7f1d1d),
            on_error_container: rgb(0xffdad6),
            success_container: rgb(0x005143),
            on_success_container: rgb(0xa7f1dc),

            warning: rgb(0xf59e0b),
            info: rgb(0x06b6d4),

            is_dark: true,
            from_system: false,
        }
    }

    /// Ajusta os papéis que a shell não emite (`warning`, `info`) ao modo do esquema, para não
    /// ficarem ilegíveis num tema claro.
    pub fn with_accents_for_mode(mut self) -> Self {
        if self.is_dark {
            self.warning = rgb(0xf59e0b);
            self.info = rgb(0x06b6d4);
        } else {
            self.warning = rgb(0xb45309);
            self.info = rgb(0x0e7490);
        }
        self
    }
}

impl Default for Roles {
    fn default() -> Self {
        Self::builtin(0)
    }
}

/// Índice do tema embutido a partir do nome gravado no config.
pub fn builtin_index_from_name(name: &str) -> i32 {
    BUILTIN_NAMES.iter().position(|n| *n == name).unwrap_or(0) as i32
}

pub fn builtin_name_from_index(index: i32) -> &'static str {
    BUILTIN_NAMES.get(index.max(0) as usize).copied().unwrap_or(BUILTIN_NAMES[0])
}

/// Índice do chip a partir do nome gravado no config, incluindo "Sistema".
pub fn index_from_name(name: &str) -> i32 {
    if name == SYSTEM_NAME {
        SYSTEM_INDEX
    } else {
        builtin_index_from_name(name)
    }
}

pub fn name_from_index(index: i32) -> &'static str {
    if index == SYSTEM_INDEX {
        SYSTEM_NAME
    } else {
        builtin_name_from_index(index)
    }
}

/// Papéis correspondentes a um índice de chip.
///
/// "Sistema" sem a shell instalada cai no tema embutido — o app nunca fica sem cor.
pub fn roles_for_index(index: i32) -> Roles {
    if index == SYSTEM_INDEX {
        from_system().unwrap_or_else(|| Roles::builtin(0))
    } else {
        Roles::builtin(index)
    }
}

// ---------------------------------------------------------------------------------------------
// Hydra Shell
// ---------------------------------------------------------------------------------------------

fn config_home() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
    })
}

/// Template nativo, registrado em `Services/Theming/TemplateRegistry.qml` da shell.
/// Traz os papéis já nomeados como os tokens — não precisa de derivação.
fn app_theme_path() -> PathBuf {
    config_home().join("s4suite/theme.json")
}

/// Sempre escrito quando a shell está instalada. 16 chaves `mXxx`, das quais os níveis de
/// superfície precisam ser derivados.
fn shell_colors_path() -> PathBuf {
    config_home().join("noctalia/colors.json")
}

/// Há alguma fonte de tema do sistema disponível nesta máquina?
pub fn system_available() -> bool {
    app_theme_path().exists() || shell_colors_path().exists()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn get_color(json: &Value, key: &str) -> Option<Color> {
    parse_hex(json.get(key)?.as_str()?)
}

/// Aceita `#rrggbb`, `rrggbb`, `#rrggbbaa` e `rrggbbaa`.
pub fn parse_hex(raw: &str) -> Option<Color> {
    let hex = raw.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some(Color::from_rgb_u8(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::from_argb_u8(byte(6)?, byte(0)?, byte(2)?, byte(4)?)),
        _ => None,
    }
}

/// Deriva os 8 níveis de superfície a partir de `surface` e `surface_variant`.
///
/// O sentido **inverte** entre claro e escuro: num esquema escuro os níveis mais altos são mais
/// claros (tom 6 → 22), num esquema claro são mais escuros (tom 98 → 90). Derivar sempre com
/// `brighter` deixaria o tema claro com a hierarquia de cima para baixo.
fn derive_surfaces(surface: Color, variant: Color, is_dark: bool) -> [Color; 8] {
    let step = |c: Color, f: f32| if is_dark { c.brighter(f) } else { c.darker(f) };
    [
        step(surface, -0.08),        // dim  (mais fundo que a superfície nos dois modos)
        surface,                     // surface
        step(surface, -0.05),        // container_lowest
        step(surface, 0.10),         // container_low
        variant,                     // container
        step(variant, 0.09),         // container_high
        step(variant, 0.20),         // container_highest
        step(variant, 0.34),         // bright
    ]
}

/// Nível 2 da cascata: o `colors.json` de 16 chaves, sempre presente com a shell instalada.
///
/// Existe para o app já combinar com o desktop **antes** de o template do S4Suite ser registrado
/// na shell. Ponte, não destino: os níveis de superfície aqui são aproximações.
fn apply_shell_colors(base: &mut Roles, json: &Value) -> bool {
    let Some(surface) = get_color(json, "mSurface") else { return false };
    let variant = get_color(json, "mSurfaceVariant").unwrap_or(surface);

    base.is_dark = is_dark_color(surface);
    let s = derive_surfaces(surface, variant, base.is_dark);
    base.surface_dim = s[0];
    base.surface = s[1];
    base.surface_container_lowest = s[2];
    base.surface_container_low = s[3];
    base.surface_container = s[4];
    base.surface_container_high = s[5];
    base.surface_container_highest = s[6];
    base.surface_bright = s[7];

    let set = |key: &str, slot: &mut Color| {
        if let Some(c) = get_color(json, key) {
            *slot = c;
        }
    };
    set("mPrimary", &mut base.primary);
    set("mOnPrimary", &mut base.on_primary);
    set("mSecondary", &mut base.secondary);
    set("mOnSecondary", &mut base.on_secondary);
    set("mTertiary", &mut base.tertiary);
    set("mOnTertiary", &mut base.on_tertiary);
    set("mError", &mut base.error);
    set("mOnError", &mut base.on_error);
    set("mOnSurface", &mut base.on_surface);
    set("mOnSurfaceVariant", &mut base.on_surface_variant);
    set("mOutline", &mut base.outline);
    set("mShadow", &mut base.shadow);

    // A shell não tem `primary_container` neste formato: o par claro/escuro do primary é
    // reconstruído a partir dele, para os botões preenchidos não perderem o contraste.
    if let Some(p) = get_color(json, "mPrimary") {
        base.primary_container = if base.is_dark { p.darker(0.55) } else { p.brighter(0.55) };
        base.on_primary_container = if base.is_dark { p.brighter(0.35) } else { p.darker(0.55) };
    }
    base.outline_variant = base.outline.darker(0.35);
    base.error_container = base.error.darker(0.55);
    base.on_error_container = base.error;
    base.success_container = base.tertiary.darker(0.55);
    base.on_success_container = base.tertiary;
    base.scrim = rgb(0x000000);
    true
}

/// Nível 3 da cascata: o `theme.json` gerado pelo template do S4Suite registrado na shell.
/// Papéis já nomeados como os tokens — sem remapeamento, sem derivação.
fn apply_app_template(base: &mut Roles, json: &Value) -> bool {
    // A sonda de modo é exata: se o valor "default" bate com o "dark", o esquema ativo é escuro.
    // Só cai na luminância quando o template é antigo e não traz a sonda.
    match (json.get("modeProbeDefault"), json.get("modeProbeDark")) {
        (Some(d), Some(dk)) => base.is_dark = d == dk,
        _ => {
            if let Some(s) = get_color(json, "colorSurface") {
                base.is_dark = is_dark_color(s);
            }
        }
    }

    let mut achou = false;
    let set = |key: &str, slot: &mut Color, achou: &mut bool| {
        if let Some(c) = get_color(json, key) {
            *slot = c;
            *achou = true;
        }
    };
    set("colorPrimary", &mut base.primary, &mut achou);
    set("colorOnPrimary", &mut base.on_primary, &mut achou);
    set("colorPrimaryContainer", &mut base.primary_container, &mut achou);
    set("colorOnPrimaryContainer", &mut base.on_primary_container, &mut achou);
    set("colorSecondary", &mut base.secondary, &mut achou);
    set("colorOnSecondary", &mut base.on_secondary, &mut achou);
    set("colorTertiary", &mut base.tertiary, &mut achou);
    set("colorOnTertiary", &mut base.on_tertiary, &mut achou);
    set("colorSurfaceDim", &mut base.surface_dim, &mut achou);
    set("colorSurface", &mut base.surface, &mut achou);
    set("colorSurfaceBright", &mut base.surface_bright, &mut achou);
    set("colorSurfaceContainerLowest", &mut base.surface_container_lowest, &mut achou);
    set("colorSurfaceContainerLow", &mut base.surface_container_low, &mut achou);
    set("colorSurfaceContainer", &mut base.surface_container, &mut achou);
    set("colorSurfaceContainerHigh", &mut base.surface_container_high, &mut achou);
    set("colorSurfaceContainerHighest", &mut base.surface_container_highest, &mut achou);
    set("colorOnSurface", &mut base.on_surface, &mut achou);
    set("colorOnSurfaceVariant", &mut base.on_surface_variant, &mut achou);
    set("colorOutline", &mut base.outline, &mut achou);
    set("colorOutlineVariant", &mut base.outline_variant, &mut achou);
    set("colorScrim", &mut base.scrim, &mut achou);
    set("colorShadow", &mut base.shadow, &mut achou);
    set("colorError", &mut base.error, &mut achou);
    set("colorOnError", &mut base.on_error, &mut achou);
    set("colorErrorContainer", &mut base.error_container, &mut achou);
    set("colorOnErrorContainer", &mut base.on_error_container, &mut achou);
    set("colorSuccessContainer", &mut base.success_container, &mut achou);
    set("colorOnSuccessContainer", &mut base.on_success_container, &mut achou);

    if !achou {
        return false;
    }

    // Um template antigo (o do sims4-mod-translator, por exemplo) não traz os extremos da rampa
    // nem os pares `*_container`. O que falta é derivado **em cadeia**, cada nível a partir do
    // anterior já resolvido — derivar todos da mesma âncora produzia um `surface_bright` mais
    // escuro que o `surface_container_high` que veio pronto no arquivo.
    let escuro = base.is_dark;
    let ausente = |k: &str| get_color(json, k).is_none();
    let acima = |c: Color, f: f32| if escuro { c.brighter(f) } else { c.darker(f) };
    let abaixo = |c: Color, f: f32| if escuro { c.darker(f) } else { c.brighter(f) };

    if ausente("colorSurfaceContainer") {
        base.surface_container = acima(base.surface, 0.10);
    }
    if ausente("colorSurfaceContainerLow") {
        base.surface_container_low = acima(base.surface, 0.05);
    }
    if ausente("colorSurfaceContainerHigh") {
        base.surface_container_high = acima(base.surface_container, 0.09);
    }
    if ausente("colorSurfaceContainerHighest") {
        base.surface_container_highest = acima(base.surface_container_high, 0.12);
    }
    if ausente("colorSurfaceContainerLowest") {
        base.surface_container_lowest = abaixo(base.surface, 0.05);
    }
    if ausente("colorSurfaceDim") {
        base.surface_dim = abaixo(base.surface_container_lowest, 0.05);
    }
    if ausente("colorSurfaceBright") {
        base.surface_bright = acima(base.surface_container_highest, 0.15);
    }
    if ausente("colorOutlineVariant") {
        base.outline_variant = abaixo(base.outline, 0.35);
    }
    if ausente("colorPrimaryContainer") {
        base.primary_container = abaixo(base.primary, 0.55);
    }
    if ausente("colorOnPrimaryContainer") {
        base.on_primary_container = acima(base.primary, 0.35);
    }
    if ausente("colorOnTertiary") {
        base.on_tertiary = abaixo(base.tertiary, 0.65);
    }
    if ausente("colorScrim") {
        base.scrim = rgb(0x000000);
    }
    true
}

/// Monta os papéis a partir das fontes da shell, do mais genérico ao mais específico.
///
/// Devolve `None` só quando nenhuma fonte existe — aí o chamador fica com o tema embutido e o app
/// se comporta exatamente como antes desta integração.
pub fn from_system() -> Option<Roles> {
    from_paths(&shell_colors_path(), &app_theme_path())
}

/// Mesma cascata de [`from_system`], com os caminhos explícitos. Pública para os testes poderem
/// exercitá-la sem depender do que está instalado na máquina que os roda.
pub fn from_paths(colors: &Path, template: &Path) -> Option<Roles> {
    // A base garante que todo papel tenha valor mesmo que a fonte não o traga: `apply` sempre
    // escreve os 28, e um papel faltante viraria resíduo do tema anterior.
    let mut roles = Roles::builtin(0);
    let mut alguma = false;

    if let Some(json) = read_json(colors) {
        alguma |= apply_shell_colors(&mut roles, &json);
    }
    if let Some(json) = read_json(template) {
        alguma |= apply_app_template(&mut roles, &json);
    }
    if !alguma {
        return None;
    }
    roles.from_system = true;
    Some(roles.with_accents_for_mode())
}

/// Observa os arquivos de tema da shell e reaplica ao vivo.
///
/// Devolve a chave que liga/desliga o acompanhamento: trocar para um tema embutido a desarma sem
/// derrubar a thread, e voltar para "Sistema" a rearma.
pub fn spawn_watcher(ui: &crate::MainWindow) -> Arc<AtomicBool> {
    use slint::ComponentHandle;

    let follow = Arc::new(AtomicBool::new(false));
    let follow_thread = Arc::clone(&follow);
    let weak = ui.as_weak();

    std::thread::spawn(move || {
        let (mut ultimo_colors, mut ultimo_template) = (None, None);
        loop {
            std::thread::sleep(POLL_INTERVAL);
            if !follow_thread.load(Ordering::Relaxed) {
                // Zera o histórico para que reativar "Sistema" force uma releitura, mesmo que
                // nada tenha mudado no disco enquanto o app seguia um tema embutido.
                ultimo_colors = None;
                ultimo_template = None;
                continue;
            }

            let colors = mtime(&shell_colors_path());
            let template = mtime(&app_theme_path());
            if colors == ultimo_colors && template == ultimo_template {
                continue;
            }
            ultimo_colors = colors;
            ultimo_template = template;

            let Some(roles) = from_system() else { continue };
            if weak.upgrade_in_event_loop(move |ui| apply(&ui, &roles)).is_err() {
                return; // janela fechada: a thread não tem mais a quem servir
            }
        }
    });

    follow
}

/// Escreve **todos** os papéis no `global M3` do Slint.
///
/// Sempre completo, nunca parcial: ver a nota em [`Roles`].
pub fn apply(ui: &crate::MainWindow, roles: &Roles) {
    use slint::ComponentHandle;
    let m3 = ui.global::<crate::M3>();

    m3.set_primary(roles.primary);
    m3.set_on_primary(roles.on_primary);
    m3.set_primary_container(roles.primary_container);
    m3.set_on_primary_container(roles.on_primary_container);

    m3.set_secondary(roles.secondary);
    m3.set_on_secondary(roles.on_secondary);
    m3.set_tertiary(roles.tertiary);
    m3.set_on_tertiary(roles.on_tertiary);

    m3.set_surface_dim(roles.surface_dim);
    m3.set_surface(roles.surface);
    m3.set_surface_bright(roles.surface_bright);
    m3.set_surface_container_lowest(roles.surface_container_lowest);
    m3.set_surface_container_low(roles.surface_container_low);
    m3.set_surface_container(roles.surface_container);
    m3.set_surface_container_high(roles.surface_container_high);
    m3.set_surface_container_highest(roles.surface_container_highest);

    m3.set_on_surface(roles.on_surface);
    m3.set_on_surface_variant(roles.on_surface_variant);
    m3.set_outline(roles.outline);
    m3.set_outline_variant(roles.outline_variant);
    m3.set_scrim(roles.scrim);
    m3.set_shadow(roles.shadow);

    m3.set_error(roles.error);
    m3.set_on_error(roles.on_error);
    m3.set_error_container(roles.error_container);
    m3.set_on_error_container(roles.on_error_container);
    m3.set_success_container(roles.success_container);
    m3.set_on_success_container(roles.on_success_container);

    m3.set_warning(roles.warning);
    m3.set_info(roles.info);

    m3.set_is_dark(roles.is_dark);
    m3.set_from_system(roles.from_system);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cada_tema_embutido_tem_superficies_distintas_e_ordenadas() {
        for idx in 0..BUILTIN_NAMES.len() as i32 {
            let r = Roles::builtin(idx);
            // A rampa precisa de fato subir de tom, senão a hierarquia de cards some.
            let niveis = [
                r.surface_container_lowest,
                r.surface,
                r.surface_container_low,
                r.surface_container,
                r.surface_container_high,
                r.surface_container_highest,
            ];
            for par in niveis.windows(2) {
                let soma = |c: Color| c.red() as u32 + c.green() as u32 + c.blue() as u32;
                assert!(
                    soma(par[0]) < soma(par[1]),
                    "tema {} tem rampa de superfície fora de ordem",
                    builtin_name_from_index(idx)
                );
            }
        }
    }

    #[test]
    fn temas_embutidos_diferem_entre_si() {
        let verde = Roles::builtin(0);
        for idx in 1..BUILTIN_NAMES.len() as i32 {
            assert_ne!(verde.primary, Roles::builtin(idx).primary);
            // O surface tint precisa propagar: dois temas não podem ter o mesmo fundo.
            assert_ne!(verde.surface, Roles::builtin(idx).surface);
        }
    }

    #[test]
    fn indice_fora_da_faixa_cai_no_primeiro_tema() {
        assert_eq!(Roles::builtin(99).primary, Roles::builtin(0).primary);
        assert_eq!(Roles::builtin(-3).primary, Roles::builtin(0).primary);
    }

    #[test]
    fn luminancia_separa_claro_de_escuro() {
        assert!(is_dark_color(rgb(0x121410)));
        assert!(is_dark_color(rgb(0x0f172a)));
        assert!(!is_dark_color(rgb(0xf8fafc)));
        assert!(!is_dark_color(rgb(0xdce7d5)));
    }
}
