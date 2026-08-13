//! Cobertura da integração com o tema do sistema (Hydra Shell).
//!
//! A falha que estes testes existem para pegar não é "a cor saiu errada" — é a **aplicação
//! parcial**: uma fonte que traz 20 dos 28 papéis, os 8 restantes ficando com o valor do tema
//! anterior. Ela não aparece na primeira troca, só ao alternar de esquema duas vezes, e por isso
//! escapa de qualquer inspeção visual apressada.

use i_slint_backend_testing as slint_testing;
use s4suite::core::theme::{self, Roles};
use s4suite::MainWindow;
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// `colors.json` real desta máquina, reduzido ao que a shell sempre escreve (16 chaves).
const COLORS_ESCURO: &str = r##"{
  "mPrimary": "#afd191", "mOnPrimary": "#1d3707",
  "mSecondary": "#bdcbad", "mOnSecondary": "#28341e",
  "mTertiary": "#8bd5c0", "mOnTertiary": "#00382d",
  "mError": "#ffb4ab", "mOnError": "#690005",
  "mSurface": "#121410", "mOnSurface": "#e3e3db",
  "mSurfaceVariant": "#1e201c", "mOnSurfaceVariant": "#c4c8ba",
  "mOutline": "#44483e", "mShadow": "#000000",
  "mHover": "#8bd5c0", "mOnHover": "#00382d"
}"##;

const COLORS_CLARO: &str = r##"{
  "mPrimary": "#3c6939", "mOnPrimary": "#ffffff",
  "mSecondary": "#52634f", "mOnSecondary": "#ffffff",
  "mTertiary": "#38656a", "mOnTertiary": "#ffffff",
  "mError": "#ba1a1a", "mOnError": "#ffffff",
  "mSurface": "#f7fbf1", "mOnSurface": "#191d17",
  "mSurfaceVariant": "#dee5d8", "mOnSurfaceVariant": "#424940",
  "mOutline": "#72796f", "mShadow": "#000000",
  "mHover": "#38656a", "mOnHover": "#ffffff"
}"##;

/// Saída do template `s4suite.json` registrado na shell.
const TEMPLATE_COMPLETO: &str = r##"{
  "colorPrimary": "#afd191", "colorOnPrimary": "#1d3707",
  "colorPrimaryContainer": "#345021", "colorOnPrimaryContainer": "#cbeeaa",
  "colorSecondary": "#bdcbad", "colorOnSecondary": "#28341e",
  "colorTertiary": "#8bd5c0", "colorOnTertiary": "#00382d",
  "colorSurfaceDim": "#121410", "colorSurface": "#121410", "colorSurfaceBright": "#383a35",
  "colorSurfaceContainerLowest": "#0d0f0b", "colorSurfaceContainerLow": "#1a1c18",
  "colorSurfaceContainer": "#1e201c", "colorSurfaceContainerHigh": "#292b26",
  "colorSurfaceContainerHighest": "#343630",
  "colorOnSurface": "#e3e3db", "colorOnSurfaceVariant": "#c4c8ba",
  "colorOutline": "#8e9285", "colorOutlineVariant": "#44483e",
  "colorScrim": "#000000", "colorShadow": "#000000",
  "colorError": "#ffb4ab", "colorOnError": "#690005",
  "colorErrorContainer": "#93000a", "colorOnErrorContainer": "#ffdad6",
  "colorSuccessContainer": "#005143", "colorOnSuccessContainer": "#a7f1dc",
  "modeProbeDefault": "#121410", "modeProbeDark": "#121410"
}"##;

struct Fontes {
    _dir: TempDir,
    colors: PathBuf,
    template: PathBuf,
}

impl Fontes {
    fn nova() -> Self {
        let dir = TempDir::new().unwrap();
        Self {
            colors: dir.path().join("colors.json"),
            template: dir.path().join("theme.json"),
            _dir: dir,
        }
    }

    fn com_colors(self, conteudo: &str) -> Self {
        std::fs::write(&self.colors, conteudo).unwrap();
        self
    }

    fn com_template(self, conteudo: &str) -> Self {
        std::fs::write(&self.template, conteudo).unwrap();
        self
    }

    fn resolver(&self) -> Option<Roles> {
        theme::from_paths(&self.colors, &self.template)
    }
}

#[test]
fn hex_de_seis_e_oito_digitos_sao_aceitos_e_o_resto_recusado() {
    let c = theme::parse_hex("#afd191").expect("6 dígitos com # deve parsear");
    assert_eq!((c.red(), c.green(), c.blue()), (0xaf, 0xd1, 0x91));

    let c = theme::parse_hex("121410aa").expect("8 dígitos sem # deve parsear");
    assert_eq!((c.red(), c.green(), c.blue(), c.alpha()), (0x12, 0x14, 0x10, 0xaa));

    assert!(theme::parse_hex("nao-e-cor").is_none());
    assert!(theme::parse_hex("#12345").is_none());
    assert!(theme::parse_hex("").is_none());
}

#[test]
fn sem_nenhuma_fonte_o_tema_do_sistema_nao_existe() {
    let fontes = Fontes::nova();
    assert!(
        fontes.resolver().is_none(),
        "sem arquivos da shell o app tem de ficar com o tema embutido, não com um tema meio pintado"
    );
}

#[test]
fn so_o_colors_json_ja_da_um_tema_completo() {
    let roles = Fontes::nova().com_colors(COLORS_ESCURO).resolver().expect("nível 2 deve resolver");

    assert!(roles.from_system);
    assert!(roles.is_dark, "#121410 é uma superfície escura");
    assert_eq!(roles.primary, theme::parse_hex("#afd191").unwrap());
    assert_eq!(roles.on_surface, theme::parse_hex("#e3e3db").unwrap());

    // O ponto do nível 2: os papéis que o `colors.json` não tem precisam sair derivados, não
    // herdados do tema embutido verde.
    assert_ne!(roles.surface_container_high, Roles::builtin(0).surface_container_high);
    assert_ne!(roles.primary_container, Roles::builtin(0).primary_container);
}

#[test]
fn o_template_do_app_tem_a_ultima_palavra_sobre_o_colors_json() {
    let roles = Fontes::nova()
        .com_colors(COLORS_ESCURO)
        .com_template(TEMPLATE_COMPLETO)
        .resolver()
        .expect("nível 3 deve resolver");

    // `colorOutline` do template (#8e9285) difere do `mOutline` do colors.json (#44483e):
    // se o nível 2 tivesse vencido, a cascata estaria invertida.
    assert_eq!(roles.outline, theme::parse_hex("#8e9285").unwrap());
    assert_eq!(roles.outline_variant, theme::parse_hex("#44483e").unwrap());
    assert_eq!(roles.primary_container, theme::parse_hex("#345021").unwrap());
    assert_eq!(roles.surface_container_highest, theme::parse_hex("#343630").unwrap());
}

#[test]
fn a_sonda_de_modo_do_template_decide_claro_ou_escuro() {
    let claro = TEMPLATE_COMPLETO
        .replace(r##""modeProbeDark": "#121410""##, r##""modeProbeDark": "#0a0a0a""##);
    let roles = Fontes::nova().com_template(&claro).resolver().unwrap();
    assert!(
        !roles.is_dark,
        "default != dark significa esquema claro, mesmo com superfícies escuras no arquivo"
    );

    let roles = Fontes::nova().com_template(TEMPLATE_COMPLETO).resolver().unwrap();
    assert!(roles.is_dark, "default == dark significa esquema escuro");
}

#[test]
fn a_rampa_de_superficie_inverte_de_sentido_no_tema_claro() {
    let soma = |c: slint::Color| c.red() as u32 + c.green() as u32 + c.blue() as u32;

    let escuro = Fontes::nova().com_colors(COLORS_ESCURO).resolver().unwrap();
    assert!(
        soma(escuro.surface) < soma(escuro.surface_container_highest),
        "no escuro os níveis altos precisam ser mais claros que a superfície"
    );

    let claro = Fontes::nova().com_colors(COLORS_CLARO).resolver().unwrap();
    assert!(!claro.is_dark, "#f7fbf1 é uma superfície clara");
    assert!(
        soma(claro.surface) > soma(claro.surface_container_highest),
        "no claro os níveis altos precisam ser mais escuros — derivar sempre com `brighter` \
         deixaria a hierarquia de cards de cabeça para baixo"
    );
}

#[test]
fn warning_e_info_acompanham_o_modo_do_esquema() {
    let escuro = Fontes::nova().com_colors(COLORS_ESCURO).resolver().unwrap();
    let claro = Fontes::nova().com_colors(COLORS_CLARO).resolver().unwrap();
    assert_ne!(
        escuro.warning, claro.warning,
        "a shell não emite `warning`; ele tem de ser reescolhido por modo, senão some no claro"
    );
    assert_ne!(escuro.info, claro.info);
}

/// O teste que justifica o resto do arquivo: aplicar um tema por cima de outro não pode deixar
/// nenhum papel do primeiro para trás.
#[test]
fn aplicar_um_tema_por_cima_de_outro_nao_deixa_residuo() {
    slint_testing::init_no_event_loop();
    let janela = MainWindow::new().unwrap();

    let claro = Fontes::nova().com_colors(COLORS_CLARO).resolver().unwrap();
    let escuro = Fontes::nova()
        .com_colors(COLORS_ESCURO)
        .com_template(TEMPLATE_COMPLETO)
        .resolver()
        .unwrap();

    // Ida e volta duas vezes: é o ciclo que expõe aplicação parcial.
    for _ in 0..2 {
        theme::apply(&janela, &claro);
        conferir(&janela, &claro);
        theme::apply(&janela, &escuro);
        conferir(&janela, &escuro);
    }

    fn conferir(janela: &MainWindow, r: &Roles) {
        let m3 = janela.global::<s4suite::M3>();
        let campos: [(&str, slint::Color, slint::Color); 30] = [
            ("primary", m3.get_primary(), r.primary),
            ("on_primary", m3.get_on_primary(), r.on_primary),
            ("primary_container", m3.get_primary_container(), r.primary_container),
            ("on_primary_container", m3.get_on_primary_container(), r.on_primary_container),
            ("secondary", m3.get_secondary(), r.secondary),
            ("on_secondary", m3.get_on_secondary(), r.on_secondary),
            ("tertiary", m3.get_tertiary(), r.tertiary),
            ("on_tertiary", m3.get_on_tertiary(), r.on_tertiary),
            ("surface_dim", m3.get_surface_dim(), r.surface_dim),
            ("surface", m3.get_surface(), r.surface),
            ("surface_bright", m3.get_surface_bright(), r.surface_bright),
            (
                "surface_container_lowest",
                m3.get_surface_container_lowest(),
                r.surface_container_lowest,
            ),
            ("surface_container_low", m3.get_surface_container_low(), r.surface_container_low),
            ("surface_container", m3.get_surface_container(), r.surface_container),
            ("surface_container_high", m3.get_surface_container_high(), r.surface_container_high),
            (
                "surface_container_highest",
                m3.get_surface_container_highest(),
                r.surface_container_highest,
            ),
            ("on_surface", m3.get_on_surface(), r.on_surface),
            ("on_surface_variant", m3.get_on_surface_variant(), r.on_surface_variant),
            ("outline", m3.get_outline(), r.outline),
            ("outline_variant", m3.get_outline_variant(), r.outline_variant),
            ("scrim", m3.get_scrim(), r.scrim),
            ("shadow", m3.get_shadow(), r.shadow),
            ("error", m3.get_error(), r.error),
            ("on_error", m3.get_on_error(), r.on_error),
            ("error_container", m3.get_error_container(), r.error_container),
            ("on_error_container", m3.get_on_error_container(), r.on_error_container),
            ("success_container", m3.get_success_container(), r.success_container),
            ("on_success_container", m3.get_on_success_container(), r.on_success_container),
            ("warning", m3.get_warning(), r.warning),
            ("info", m3.get_info(), r.info),
        ];
        for (nome, na_ui, esperado) in campos {
            assert_eq!(na_ui, esperado, "papel `{}` ficou com resíduo do tema anterior", nome);
        }
        assert_eq!(m3.get_is_dark(), r.is_dark);
        assert_eq!(m3.get_from_system(), r.from_system);
    }
}

/// A troca pelo chip precisa repintar na hora, como já acontece com os temas embutidos.
#[test]
fn o_indice_do_chip_sistema_resolve_para_a_shell_quando_ela_existe() {
    assert_eq!(theme::index_from_name(theme::SYSTEM_NAME), theme::SYSTEM_INDEX);
    assert_eq!(theme::name_from_index(theme::SYSTEM_INDEX), theme::SYSTEM_NAME);
    assert_eq!(theme::name_from_index(2), "Cyber Purple");

    // Nome desconhecido no config (arquivo editado à mão, versão antiga) cai no primeiro tema.
    assert_eq!(theme::index_from_name("Tema Que Nao Existe"), 0);
}

#[test]
fn template_antigo_sem_os_containers_ainda_rende_uma_rampa_completa() {
    // É exatamente o formato do template do sims4-mod-translator: sem `SurfaceContainerLowest`,
    // sem `SurfaceDim`, sem `SurfaceBright`.
    let parcial = r##"{
      "colorSurface": "#121410", "colorSurfaceContainerLow": "#1a1c18",
      "colorSurfaceContainer": "#1e201c", "colorSurfaceContainerHigh": "#292b26",
      "colorOnSurface": "#e3e3db", "colorPrimary": "#afd191"
    }"##;
    let roles = Fontes::nova().com_template(parcial).resolver().unwrap();
    let soma = |c: slint::Color| c.red() as u32 + c.green() as u32 + c.blue() as u32;

    assert!(
        soma(roles.surface_container_lowest) < soma(roles.surface),
        "o nível mais fundo tem de ser derivado, não herdado do tema embutido"
    );
    assert!(soma(roles.surface_bright) > soma(roles.surface_container_high));
}

#[test]
fn caminhos_inexistentes_nao_derrubam_a_resolucao() {
    assert!(theme::from_paths(Path::new("/nao/existe/colors.json"), Path::new("/nao/existe/t.json"))
        .is_none());
}
