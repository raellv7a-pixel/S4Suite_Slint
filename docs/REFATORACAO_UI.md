# Plano de Refatoração de UI — Material Design 3 no Slint

> **Estado da execução (2026-08-09).** Etapas 1–11 implementadas; 12 e 13 pendentes.
> Ver §13 para o que mudou em relação ao plano durante a execução e o que ficou de fora.


> Base: app executado em 2026-08-09 (`target/debug/s4suite`, Wayland/Hyprland, janela 934×1019).
> Todos os construtos Slint citados aqui foram **compilados neste repositório** antes de entrar no
> plano (ver §10). O `Cargo.lock` resolve `slint 1.17.1`, embora o `Cargo.toml` peça `1.8`.

---

## 1. Diagnóstico do estado atual

O app funciona e a arquitetura de views é sólida (uma `.slint` por aba, props/callbacks bem
tipados). O problema é **puramente de camada visual e de movimento**, e ele é mensurável:

```
$ grep -rn "animate"      ui/   →  1 ocorrência (merger.slint:309)
$ grep -rn "drop-shadow"  ui/   →  0
$ grep -rn "states \["    ui/   →  0
```

Uma animação, zero elevação, zero máquinas de estado em 2.316 linhas de UI. É isso que faz a
interface parecer "chapada" apesar da paleta ser agradável.

### 1.1 Problemas visuais confirmados nos prints

| # | Problema | Onde | Evidência |
|---|---|---|---|
| P1 | **Ícones duplicados nos botões** | Dashboard, Tray, Traduções | `PrimaryButton` renderiza `icon_str` **e** o `text`, mas o `text` já traz o emoji: `icon_str: "🧹"` + `text: "🧹 Limpar Cache do Jogo"` → sai `🧹 🧹 Limpar Cache do Jogo` (`ui/views/dashboard.slint:157-177`) |
| P2 | **Emoji quebrado (tofu `□`)** | Sidebar (Tray, Configurações), Dashboard (Tamanho, Scripts, Backup), títulos de Tray/Config | `NotoColorEmoji` existe no sistema, mas o fallback do Slint não cobre sequências com VS16 (`🗂️`, `⚙️`, `🛡️`, `ℹ️`). Emojis sem VS16 (`📦`, `📂`, `🔗`, `🌐`) renderizam |
| P3 | **Toolbar vazando pela borda direita** | Organizador ("Ver Desa…", "Na…"), Tray ("Destino do CC…"), Dashboard ("Atualizar Estatísticas") | `HorizontalLayout` sem wrap, sem scroll e sem largura mínima negociada |
| P4 | **Cards sem hierarquia** | todas as views | `Card` = `Rectangle` + `border-radius: 12px` + borda 1px. Todos os cards têm exatamente o mesmo peso visual, do banner de boas-vindas ao rodapé de status |
| P5 | **Troca de aba sem transição** | `ui/main.slint:182-337` | `if root.current_tab == N : View` **destrói e recria** a view. Além de impedir animação, perde posição de scroll e estado local (ex.: `open_details` do dashboard) |
| P6 | **Diálogos aparecem "estalando"** | dashboard, organizer, installer, merger | scrim (`#000000.transparentize(0.4)`) e card entram com opacidade e escala finais, sem fade nem scale-in |
| P7 | **Feedback de clique inexistente** | todos os botões | só há `has-hover` trocando `background`. Sem estado `pressed`, sem state layer, sem ripple |
| P8 | **Tipografia ad-hoc** | todas as views | 10 tamanhos distintos codificados na mão (10, 11, 12, 13, 14, 16, 18, 20, 26, 28, 36 px) e pesos 500/600/700/800 sem regra |
| P9 | **Paleta invertida para tema escuro** | `ui/palette.slint:6-8` | `primary_color: #15803d` é um verde **escuro** usado como preenchimento sobre superfície escura. Em MD3 dark, `primary` é o tom **claro** (~tone 80) e o tom escuro vira `primary-container`. Daí o botão verde "pesar" contra o fundo |
| P10 | **Sem foco de teclado / acessibilidade** | todo o app | nenhum `FocusScope`, nenhuma propriedade `accessible-*`. O app é 100% dependente de mouse |
| P11 | **Barra de status duplicada** | installer, organizer, merger, tray, translations, config | o mesmo `Rectangle` + `Text` de status reescrito em 6 arquivos |
| P12 | **Cards estáticos** | Dashboard | `height: 110px` fixo, valor sem animação ao atualizar, sem estado de "carregando"/"vazio"/"erro" |

---

## 2. O alvo

Material Design 3, adaptado ao que o Slint entrega nativamente — **sem** tentar clonar o Compose.
Quatro pilares:

1. **Material Design 3** — sistema de tokens (cor por *role*, tipografia, forma, elevação) em vez
   de valores soltos.
2. **Animações fluidas** — todo estado visível transiciona; nada troca em 1 frame.
3. **Cards dinâmicos** — o card reage a hover/press/seleção/carregamento e cresce com o conteúdo.
4. **Tema do sistema (Hydra Shell)** — o app segue as cores geradas pela shell ao vivo, como
   cidadão nativo do desktop, com os temas embutidos como fallback (§4).

Restrição de projeto: **nenhuma mudança na fiação UI↔engine**. As propriedades e callbacks de
`MainWindow` (`ui/main.slint:21-168`) e o `slint_adapter.rs` ficam intactos — a refatoração é da
camada de apresentação para dentro. Isso mantém `tests/bridge_wiring.rs` verde o tempo todo e é a
principal defesa contra o bug recorrente de "callback que não chega ao motor".

---

## 3. Fase 0 — Fundação de tokens

Substitui `ui/palette.slint` por três globais. Nenhuma view muda ainda; só ganham vocabulário.

### 3.1 `ui/theme/colors.slint` — roles MD3

O erro estrutural de hoje é ter 24 cores nomeadas por *aparência* (`surface_header`,
`surface_hover`). MD3 nomeia por *papel*, o que permite trocar o tema inteiro sem tocar nas views.

**Decisão de arquitetura (imposta por §4):** os tokens são `in-out` com valores **constantes** de
fallback, e **quem resolve o tema é o Rust**, não expressões no `.slint`. A tentação natural seria
escrever `out property <color> primary: scheme == 0 ? #4ade80 : ...` — mas em Slint, quando o Rust
escreve numa propriedade, a *binding* declarativa é descartada em definitivo. Um token com
expressão ternária deixaria de reagir ao seletor de tema para sempre depois da primeira aplicação
do tema do sistema. Portanto: os 4 temas embutidos migram para uma tabela em Rust
(`src/core/theme.rs`), e o caminho de código é **idêntico** para tema embutido e tema da shell.

```slint
// ui/theme/colors.slint
// Valores abaixo = fallback embutido (Sims Green dark). São sobrescritos em runtime
// por src/core/theme.rs, tanto para os temas embutidos quanto para a Hydra Shell (§4).
export global M3 {
    // --- Primary: em dark scheme o "primary" é o tom CLARO (P9) ---
    in-out property <color> primary:               #4ade80;
    in-out property <color> on_primary:            #00391a;
    in-out property <color> primary_container:     #15803d;
    in-out property <color> on_primary_container:  #b9f6ca;

    in-out property <color> secondary:             #bccfc2;
    in-out property <color> on_secondary:          #26372c;
    in-out property <color> tertiary:              #a0cfd4;
    in-out property <color> on_tertiary:           #00363b;

    // --- Superfícies em níveis (o que dá profundidade sem sombra) ---
    in-out property <color> surface_dim:               #0b1020;
    in-out property <color> surface:                   #0f172a;   // = background atual
    in-out property <color> surface_bright:            #2a3550;
    in-out property <color> surface_container_lowest:  #0a0f1c;
    in-out property <color> surface_container_low:     #141c30;
    in-out property <color> surface_container:         #182136;
    in-out property <color> surface_container_high:    #222d45;
    in-out property <color> surface_container_highest: #2c3852;

    in-out property <color> on_surface:          #e8eaf0;
    in-out property <color> on_surface_variant:  #b7c0d0;
    in-out property <color> outline:             #7c8798;
    in-out property <color> outline_variant:     #333f55;
    in-out property <color> scrim:               #000000;
    in-out property <color> shadow:              #000000;

    // --- Semânticos ---
    in-out property <color> error:                #ef4444;
    in-out property <color> on_error:             #450a0a;
    in-out property <color> error_container:      #93000a;
    in-out property <color> on_error_container:   #ffdad6;
    in-out property <color> success_container:    #005143;
    in-out property <color> on_success_container: #a7f1dc;
    in-out property <color> warning:              #f59e0b;
    in-out property <color> info:                 #06b6d4;

    // --- Metadados do esquema ativo (usados por §4) ---
    in-out property <bool> is_dark: true;
    in-out property <bool> from_system: false;

    // --- Constantes de state layer (essas podem ser `out`: nunca vêm do Rust) ---
    out property <float> layer_hover:   0.08;
    out property <float> layer_focus:   0.10;
    out property <float> layer_pressed: 0.10;
    out property <float> layer_drag:    0.16;
}
```

**Ganho imediato:** os 6 níveis de `surface_container_*` resolvem P4 sem uma linha de sombra.
Banner de boas-vindas em `surface_container_low`, cards de métrica em `surface_container`,
diálogo em `surface_container_high`, menu/popup em `surface_container_highest`.

**Nomes não são livres.** Cada `in-out property` acima corresponde 1:1 a um *role* que a Hydra
Shell já gera (§4.1). Não inventar nomes fora dessa lista — um token sem contraparte na shell vira
um buraco que precisa de derivação manual a cada troca de esquema.

**Compatibilidade:** manter `export global Theme` como *alias fino* apontando para `M3`
(`out property <color> surface: M3.surface_container;` etc.) para as 9 views migrarem uma a uma
em vez de tudo num commit só.

### 3.2 `ui/theme/motion.slint` — durações e curvas MD3

```slint
export global Motion {
    // Durações
    out property <duration> short2:  100ms;
    out property <duration> short4:  200ms;
    out property <duration> medium2: 300ms;
    out property <duration> medium4: 400ms;
    out property <duration> long2:   500ms;

    // Curvas (verificadas: `easing` é tipo válido de propriedade)
    out property <easing> emphasized:            cubic-bezier(0.2, 0.0, 0.0, 1.0);
    out property <easing> emphasized_decelerate: cubic-bezier(0.05, 0.7, 0.1, 1.0);
    out property <easing> emphasized_accelerate: cubic-bezier(0.3, 0.0, 0.8, 0.15);
    out property <easing> standard:              cubic-bezier(0.2, 0.0, 0.0, 1.0);
}
```

Regra de uso, para não virar bagunça:

| Situação | Duração | Curva |
|---|---|---|
| State layer, cor de botão, hover | `short2` (100ms) | `standard` |
| Elevação de card, seleção | `short4` (200ms) | `emphasized` |
| Entrada de diálogo / painel | `medium4` (400ms) | `emphasized_decelerate` |
| Saída de diálogo / painel | `short4` (200ms) | `emphasized_accelerate` |
| Transição de aba | `medium2` (300ms) | `emphasized` |

Entrada é sempre mais lenta que a saída. É o que separa "fluido" de "lento".

### 3.3 `ui/theme/typography.slint` e `shape.slint`

```slint
export global Type {
    out property <length> display_s: 36px;   out property <int> display_s_w: 400;
    out property <length> headline_s: 24px;  out property <int> headline_s_w: 400;
    out property <length> title_l: 22px;     out property <int> title_l_w: 500;
    out property <length> title_m: 16px;     out property <int> title_m_w: 600;
    out property <length> title_s: 14px;     out property <int> title_s_w: 600;
    out property <length> body_l: 16px;      out property <int> body_l_w: 400;
    out property <length> body_m: 14px;      out property <int> body_m_w: 400;
    out property <length> body_s: 12px;      out property <int> body_s_w: 400;
    out property <length> label_l: 14px;     out property <int> label_l_w: 600;
    out property <length> label_m: 12px;     out property <int> label_m_w: 600;
    out property <length> label_s: 11px;     out property <int> label_s_w: 600;
}

export global Shape {
    out property <length> xs:  4px;
    out property <length> sm:  8px;
    out property <length> md: 12px;
    out property <length> lg: 16px;
    out property <length> xl: 28px;   // diálogos
}

export global Elevation {
    out property <length> l0: 0px;  out property <length> l1:  2px;
    out property <length> l2: 6px;  out property <length> l3: 12px;
    out property <length> l4: 16px; out property <length> l5: 24px;   // raio de blur
}
```

Isso mata P8: os 11 tamanhos soltos viram 11 *papéis*, e um `find`/`replace` guiado resolve as views.

---

## 4. Fase 0.5 — Integração com o tema do sistema (Hydra Shell)

Referência de implementação: `/home/raell/sims4-mod-translator` (`src/theme.rs` + `ui/theme.slint`).
**Copiar o mecanismo, não o desenho.** O Translator tem uma UI deliberadamente simples porque é um
app de uma tela; o S4Suite fica com tudo do §3 ao §9 e apenas *ganha* a fonte de cor da shell.

### 4.1 Como a shell publica as cores (pipeline verificado)

```
wallpaper / esquema escolhido
        │
        ▼
Scripts/python/src/theming/template-processor.py   (renderer compatível com Matugen)
        │  --default-mode {dark|light}   ← modo ativo de Settings.data.colorSchemes.darkMode
        ▼
Assets/Templates/<app>.json   +   Services/Theming/TemplateRegistry.qml
        │
        ├─► ~/.config/noctalia/colors.json          (sempre escrito — 16 chaves `mXxx`)
        └─► ~/.config/<app>/theme.json              (por app registrado, papéis já nomeados)
```

O ponto forte, que muda o cálculo deste plano: `lib/material.py` já emite o **conjunto MD3
completo** — `primary_container`, `on_primary_container`, `surface_container_lowest/low/…/highest`,
`surface_dim`, `surface_bright`, `outline_variant`, `scrim`, `shadow`, `inverse_*`, `*_fixed`.
Ou seja, **todos os tokens do §3.1 têm contraparte nativa na shell**. Nenhum precisa ser derivado
com `.darker()`/`.brighter()` como o Translator faz no seu fallback — aquilo lá é uma concessão ao
`colors.json` de 16 chaves, não o caminho principal.

Sintaxe de template: `{{colors.<role>.<mode>.hex}}`, com `<mode>` ∈ `default` | `dark` | `light`.
`default` resolve para o modo ativo, porque a shell sempre invoca o processador com
`--default-mode <modo ativo>` (`Services/Theming/TemplateProcessor.qml:163,453,478,500`).

### 4.2 O template do S4Suite

Dois arquivos a criar **do lado da hydra-shell** (não deste repositório):

**a) `Assets/Templates/s4suite.json`**

```jsonc
{
  "colorPrimary":                "{{colors.primary.default.hex}}",
  "colorOnPrimary":              "{{colors.on_primary.default.hex}}",
  "colorPrimaryContainer":       "{{colors.primary_container.default.hex}}",
  "colorOnPrimaryContainer":     "{{colors.on_primary_container.default.hex}}",

  "colorSecondary":              "{{colors.secondary.default.hex}}",
  "colorOnSecondary":            "{{colors.on_secondary.default.hex}}",
  "colorTertiary":               "{{colors.tertiary.default.hex}}",
  "colorOnTertiary":             "{{colors.on_tertiary.default.hex}}",

  "colorSurfaceDim":             "{{colors.surface_dim.default.hex}}",
  "colorSurface":                "{{colors.surface.default.hex}}",
  "colorSurfaceBright":          "{{colors.surface_bright.default.hex}}",
  "colorSurfaceContainerLowest": "{{colors.surface_container_lowest.default.hex}}",
  "colorSurfaceContainerLow":    "{{colors.surface_container_low.default.hex}}",
  "colorSurfaceContainer":       "{{colors.surface_container.default.hex}}",
  "colorSurfaceContainerHigh":   "{{colors.surface_container_high.default.hex}}",
  "colorSurfaceContainerHighest":"{{colors.surface_container_highest.default.hex}}",

  "colorOnSurface":              "{{colors.on_surface.default.hex}}",
  "colorOnSurfaceVariant":       "{{colors.on_surface_variant.default.hex}}",
  "colorOutline":                "{{colors.outline.default.hex}}",
  "colorOutlineVariant":         "{{colors.outline_variant.default.hex}}",
  "colorScrim":                  "{{colors.scrim.default.hex}}",
  "colorShadow":                 "{{colors.shadow.default.hex}}",

  "colorError":                  "{{colors.error.default.hex}}",
  "colorOnError":                "{{colors.on_error.default.hex}}",
  "colorErrorContainer":         "{{colors.error_container.default.hex}}",
  "colorOnErrorContainer":       "{{colors.on_error_container.default.hex}}",
  "colorSuccessContainer":       "{{colors.tertiary_container.default.hex}}",
  "colorOnSuccessContainer":     "{{colors.on_tertiary_container.default.hex}}",

  // Sonda de modo: o app compara `default` com `dark`; se forem iguais, está em dark.
  // Exato, sem heurística de luminância.
  "modeProbeDefault":            "{{colors.surface.default.hex}}",
  "modeProbeDark":               "{{colors.surface.dark.hex}}"
}
```

> Sem comentários no arquivo real — `jsonc` acima é só para leitura.

**b) entrada em `Services/Theming/TemplateRegistry.qml`**, no mesmo formato do Translator
(que está em `TemplateRegistry.qml:91-101`):

```qml
{
  "id": "s4suite",
  "name": "S4Suite",
  "category": "system",
  "input": "s4suite.json",
  "outputs": [
    { "path": "~/.config/s4suite/theme.json" }
  ]
},
```

### 4.3 Cascata de fontes (do mais específico ao mais genérico)

O app tenta, nesta ordem, e **cada nível só sobrescreve os papéis que conhece**:

| # | Fonte | Cobertura | Quando existe |
|---|---|---|---|
| 3 | `~/.config/s4suite/theme.json` | 28 papéis, nomeados igual aos tokens | shell instalada **e** template registrado |
| 2 | `~/.config/noctalia/colors.json` | 16 chaves `mXxx`; os níveis de superfície são derivados | shell instalada (sempre) |
| 1 | tabela embutida em `src/core/theme.rs` | 100% | sempre |

O nível 2 existe para o app já ficar coerente com o desktop **antes** de o template ser
registrado — foi essa a escolha do Translator e ela se paga. O mapeamento derivado, copiado de
`sims4-mod-translator/src/theme.rs:139-174`:

```
surface                 ← mSurface
on_surface              ← mOnSurface
surface_container       ← mSurfaceVariant
on_surface_variant      ← mOnSurfaceVariant
outline                 ← mOutline
surface_container_low   ← mSurface.darker(0.12)
surface_container_high  ← mSurfaceVariant.brighter(0.08)
surface_dim             ← mSurface.darker(0.35)
primary / on_primary    ← mPrimary / mOnPrimary
error_container         ← mError.darker(0.55)
success_container       ← mTertiary.darker(0.55)
```

**Importante:** essas derivações assumem esquema escuro. No nível 2 com tema claro elas invertem o
sentido (`darker` sobre uma superfície clara continua funcionando, mas `surface_dim` fica exagerado).
Como o nível 2 é ponte, não destino, aceitar a imprecisão e priorizar o registro do template.

### 4.4 `src/core/theme.rs` — resolvedor único

Um módulo novo, **fora** de `src/bridge/`, porque não é fiação UI↔engine: é fonte de dados de
apresentação. Nenhuma dependência nova (`serde_json` e `dirs` já estão no `Cargo.toml`).

```rust
pub enum ThemeSource {
    Builtin(BuiltinScheme),   // SimsGreen | DeepBlue | CyberPurple | NeonPink
    System,                   // Hydra Shell
}

/// Conjunto completo de papéis MD3. Um só tipo para os dois caminhos.
pub struct Roles { /* 28 campos Color + is_dark: bool */ }

impl Roles {
    pub fn builtin(scheme: BuiltinScheme) -> Self { /* tabela const */ }
    pub fn from_system() -> Option<Self> { /* cascata 3 → 2, None se nenhuma */ }
}

/// Escreve TODOS os papéis no `global M3`. Chamada única, sempre completa —
/// nunca aplicar um subconjunto, senão sobram cores do tema anterior.
pub fn apply(ui: &MainWindow, roles: &Roles);

/// Observa os arquivos da shell e reaplica ao vivo. Só roda com ThemeSource::System.
pub fn spawn_watcher(ui: &MainWindow);
```

`spawn_watcher` segue o padrão do Translator: `std::thread::spawn` + polling de `mtime` a cada
~700ms nos dois caminhos, e `weak.upgrade_in_event_loop(...)` para tocar a UI. Vale manter o
polling em vez de trazer o crate `notify` — este projeto já usa `spawn_blocking`/threads para todo
o trabalho de fundo, e uma dependência a menos num app que é distribuído como AppImage é lucro.

**Regra que evita o bug clássico:** `apply()` escreve os 28 papéis sempre, mesmo os que a fonte
não trouxe (preenchidos a partir da tabela embutida). Aplicar só o subconjunto encontrado deixa
resíduo do tema anterior — é o tipo de falha que aparece só ao alternar dark↔light duas vezes.

### 4.5 Detecção de claro/escuro

`M3.is_dark` alimenta decisões que não são só de cor: opacidade de scrim (0.4 no escuro, 0.32 no
claro), tint do `StateLayer` (`on_surface` nos dois casos, mas com opacidades diferentes) e a cor
das sombras (no tema claro, `drop-shadow-color` precisa ser bem mais transparente ou vira sujeira).

- **Nível 3:** exato — comparar `modeProbeDefault` com `modeProbeDark`; iguais ⇒ dark.
- **Nível 2 e fallback:** luminância relativa de `surface` (`0.2126·R + 0.7152·G + 0.0722·B < 0.5`).

### 4.6 Convivência com o seletor de tema do app

A tela de Configurações já tem 4 chips de tema (`Sims Green`, `Deep Blue`, `Cyber Purple`,
`Neon Pink`) ligados a `theme_selected(int)` → `cfg.theme: String`
(`src/bridge/slint_adapter.rs:2063-2074`, `src/core/config.rs:11`). A integração adiciona um
**5º chip: “Sistema (Hydra Shell)”**.

- `cfg.theme` já é `String`, então `"System"` entra sem migração de schema — `theme_index_from_name`
  (`slint_adapter.rs:2146`) só ganha um braço.
- **Padrão inteligente na primeira execução:** se `~/.config/s4suite/theme.json` ou
  `~/.config/noctalia/colors.json` existir e o usuário nunca escolheu tema, iniciar em `"System"`.
  É o comportamento que faz o app "simplesmente combinar" ao ser instalado.
- O chip “Sistema” fica **desabilitado com dica** quando nenhum dos dois arquivos existe, em vez
  de sumir — assim o usuário descobre o recurso.
- Trocar para um tema embutido **desliga o watcher** (ou faz `apply()` ignorá-lo); trocar de volta
  para “Sistema” reaplica na hora.

### 4.7 A troca de tema tem que ser animada

É aqui que §4 encontra §6. O Translator anima `background`/`color`/`border-color` em
praticamente todo elemento temático (`ui/components.slint`, 12 ocorrências), e é isso que faz a
troca de wallpaper na shell "escorrer" pelo app em vez de piscar. No S4Suite:

```slint
// Em Card, StateLayer, NavigationRail, botões, Text de destaque…
animate background, border-color, drop-shadow-color {
    duration: Motion.medium2;          // 300ms
    easing: Motion.standard;
}
```

Como os tokens do §3.1 são `in-out` escritos pelo Rust, **toda** propriedade ligada a eles herda a
transição automaticamente — um `animate` por componente cobre o app inteiro. Nos `Text`, animar
`color` também (é barato e a ausência salta aos olhos quando o resto transiciona).

### 4.8 O que **não** trazer do Translator

| Aspecto | Translator | S4Suite |
|---|---|---|
| Densidade | tela única, poucos controles | 8 views, listas grandes — manter §7/§9 |
| Camadas de superfície | 4 níveis | 8 níveis (`surface_dim` … `container_highest`), usados para hierarquia |
| Elevação | quase só `border-color` | sombras animadas por estado (§7.1) |
| Movimento | `ease-out` 150–250ms em tudo | escala `Motion` com curvas MD3 distintas para entrada/saída (§3.2) |
| Papéis de cor | 18 | 28, incluindo `*_container` e `outline_variant` |

O que se copia é **só** o mecanismo: cascata de fontes, polling de mtime, `upgrade_in_event_loop`,
parser de hex 6/8 dígitos e a filosofia de “sem shell instalada, nada muda”.

### 4.9 Testes

O Translator cobre isso com dois testes unitários (`src/theme.rs:205-246`) e vale replicar:

1. `parse_hex` aceita 6 e 8 dígitos e rejeita lixo.
2. `Roles::from_system()` sobre um `colors.json` e um `theme.json` de exemplo em `tempfile`
   devolve o conjunto **completo** de 28 papéis.
3. **Adicional para o S4Suite:** `apply()` seguido de `apply()` com outra fonte não deixa nenhum
   papel do primeiro tema — o teste que pega o bug do §4.4.

`tempfile` já é dependência de dev e de runtime aqui, então nada novo entra.

---

## 5. Fase 1 — Componentes base

### 4.1 `StateLayer` — a peça que faz tudo parecer material

Um retângulo de cor sobreposto, com opacidade animada. É o que MD3 usa em botão, card, item de
lista e navegação — um componente resolve P7 no app inteiro.

```slint
export component StateLayer inherits Rectangle {
    in property <color> tint: M3.on_surface;
    in property <bool> hovered: false;
    in property <bool> pressed: false;
    in property <bool> focused: false;

    background: tint;
    opacity: pressed  ? M3.layer_pressed
           : focused  ? M3.layer_focus
           : hovered  ? M3.layer_hover
           : 0.0;

    animate opacity { duration: Motion.short2; easing: Motion.standard; }
}
```

### 4.2 `Ripple` — realimentação de clique a partir do ponto tocado

`TouchArea` expõe `pressed-x` / `pressed-y`, e `Timer` (verificado) dá o *release* da animação.

```slint
export component Ripple inherits Rectangle {
    in property <length> origin_x;
    in property <length> origin_y;
    in property <color> tint: #ffffff;
    property <bool> expanded: false;
    property <length> max_r: max(root.width, root.height) * 1.4;

    public function burst(px: length, py: length) {
        self.origin_x = px; self.origin_y = py;
        self.expanded = false;
        self.expanded = true;
        reset.running = true;
    }

    clip: true;

    circle := Rectangle {
        x: root.origin_x - self.width / 2;
        y: root.origin_y - self.height / 2;
        width:  root.expanded ? root.max_r : 0px;
        height: root.expanded ? root.max_r : 0px;
        border-radius: self.width / 2;
        background: root.tint;
        opacity: root.expanded ? 0.0 : 0.24;

        animate width, height { duration: Motion.medium4; easing: Motion.emphasized_decelerate; }
        animate opacity       { duration: Motion.medium4; easing: Motion.standard; }
    }

    reset := Timer {
        interval: Motion.medium4;
        running: false;
        triggered => { root.expanded = false; self.running = false; }
    }
}
```

> **Atenção de renderização:** `clip: true` recorta o *drop-shadow* do mesmo elemento. Onde houver
> ripple **e** elevação, use dois retângulos: o externo carrega `drop-shadow-*`, o interno carrega
> `clip: true` + ripple.

### 4.3 Família de botões MD3

`PrimaryButton` (`ui/components/button.slint`) vira quatro variantes com API comum, e o `icon_str`
passa a aceitar `@image-url` de SVG em vez de emoji — resolve P1 e P2 de uma vez:

| Variante | Container | Texto | Elevação | Uso no app |
|---|---|---|---|---|
| `FilledButton` | `M3.primary` | `M3.on_primary` | 0 → 1 no hover | "Iniciar Instalação", "Salvar Configurações" |
| `TonalButton` | `M3.surface_container_highest` | `M3.on_surface` | 0 | "Limpar Fila", "Backup de Saves" |
| `OutlinedButton` | transparente + borda `M3.outline` | `M3.primary` | 0 | ações secundárias da toolbar do Organizador |
| `TextButton` | transparente | `M3.primary` | 0 | "Fechar", "Cancelar" nos diálogos |

Estado `enabled: false` em MD3 = container `on_surface` a 12% e rótulo `on_surface` a 38% —
melhor que o `surface_header` cinza atual, que hoje se confunde com o `TonalButton` habilitado
(visível no print do Tray: "Iniciar Importação em Lote" desabilitado parece clicável).

### 4.4 `NavigationRail` — a sidebar

A sidebar hoje é `Rectangle` + `HorizontalLayout` com `background` trocando seco. Em MD3 o
indicador ativo é uma **pílula que desliza**, não um retângulo que aparece:

```slint
export component NavigationRail inherits Rectangle {
    in-out property <int> current_tab: 0;
    in property <length> item_h: 56px;
    in property <length> header_h: 84px;

    // Indicador único, animado — em vez de 8 backgrounds independentes
    indicator := Rectangle {
        x: 12px;
        y: root.header_h + root.current_tab * (root.item_h + 4px);
        width: root.width - 24px;
        height: root.item_h;
        border-radius: root.item_h / 2;              // pílula MD3
        background: M3.primary_container;
        animate y { duration: Motion.medium2; easing: Motion.emphasized; }
    }
    // ... itens por cima, com StateLayer e texto/ícone colorizados
}
```

Um `animate y` e a navegação inteira ganha vida. É a mudança de maior retorno por linha do plano.

### 4.5 `StatusBar` — elimina P11

Componente único (`ui/components/status_bar.slint`) com fade no texto:

```slint
export component StatusBar inherits Rectangle {
    in property <string> text;
    in property <bool> busy: false;
    // barra indeterminada quando `busy`, texto com animate opacity na troca
}
```

Substitui o bloco repetido em installer, organizer, merger, tray, translations e config.

---

## 6. Fase 2 — Animações fluidas

### 5.1 Transição de aba (resolve P5)

O `if root.current_tab == N` precisa sair. Duas opções, em ordem de preferência:

**Opção A — crossfade com opacidade (recomendada).** Mantém as views vivas, preserva scroll e
estado local, e o custo é aceitável: as views só constroem seus `for` a partir de modelos que já
vêm prontos do Rust.

```slint
Rectangle {
    horizontal-stretch: 1;
    clip: true;

    for view_idx in 8 : Rectangle {
        visible: root.current_tab == view_idx || fade.opacity > 0.01;
        // ...
    }
}
```

Na prática, um wrapper por view:

```slint
component ViewSlot inherits Rectangle {
    in property <bool> active: false;
    opacity: active ? 1.0 : 0.0;
    y: active ? 0px : 12px;                 // sobe ao entrar
    visible: self.opacity > 0.01;

    animate opacity { duration: Motion.medium2; easing: Motion.emphasized; }
    animate y       { duration: Motion.medium2; easing: Motion.emphasized_decelerate; }
}
```

**Opção B — manter o `if`, animar só a entrada.** Mais barata em memória, mas continua perdendo
scroll e estado. Só vale se a Opção A mostrar custo de RAM real com bibliotecas grandes de mods.

> **Decisão que fica para a implementação:** medir o consumo do Organizador com ~5.000 entradas
> antes de fixar A ou B. Enquanto não medir, seguir com A.

### 5.2 Diálogos (resolve P6)

Todos os overlays (`open_details`, `show_organizer_confirm`, `show_folder_picker`,
`show_careful_scan_dialog`, `show_exclusive_dialog`, `show_post_merge_dialog`,
`show_disabled_panel`) passam por um componente único:

```slint
export component Scrim inherits Rectangle {
    in property <bool> open: false;
    background: M3.scrim;
    opacity: open ? 0.4 : 0.0;
    visible: self.opacity > 0.005;
    animate opacity { duration: Motion.short4; easing: Motion.standard; }
    TouchArea { }   // bloqueia cliques atrás
}

export component Dialog inherits Rectangle {
    in property <bool> open: false;
    background: M3.surface_container_high;
    border-radius: Shape.xl;              // 28px — diálogo MD3
    drop-shadow-blur: Elevation.l3;
    drop-shadow-color: #00000099;
    drop-shadow-offset-y: 6px;

    opacity: open ? 1.0 : 0.0;
    // "scale" via largura/altura relativas, já que Slint não tem transform-scale em Rectangle
    y: open ? 0px : 24px;

    animate opacity { duration: open ? Motion.medium4 : Motion.short4;
                      easing:   open ? Motion.emphasized_decelerate : Motion.emphasized_accelerate; }
    animate y       { duration: open ? Motion.medium4 : Motion.short4;
                      easing:   open ? Motion.emphasized_decelerate : Motion.emphasized_accelerate; }
}
```

### 5.3 Progresso e números

- **Barra do merger** (`merger.slint:309`, único `animate` atual): trocar `duration: 150ms` por
  `Motion.short4` + `Motion.emphasized` e adicionar uma faixa indeterminada enquanto
  `is_merging && merge_progress == 0`.
- **Contadores do dashboard**: hoje mudam de "0" para "1238" instantaneamente. Adicionar
  `animate opacity` no `Text` durante o refresh (fade-out → troca → fade-in) já entrega a sensação
  de atualização sem precisar interpolar o número (o valor chega como `string` do Rust).

### 5.4 Entrada escalonada de listas

Nos `for` de fila (installer, merger, tray), atrasar a entrada por índice dá o "stagger" MD3:

```slint
for item[i] in root.queue_items : ListItem {
    opacity: 0;
    init => { entrada.running = true; }
    entrada := Timer {
        interval: 20ms * min(i, 12);   // teto de 12 para listas longas não arrastarem
        running: false;
        triggered => { parent.opacity = 1.0; self.running = false; }
    }
    animate opacity { duration: Motion.medium2; easing: Motion.emphasized_decelerate; }
}
```

Aplicar **apenas** onde a lista é curta e o usuário acabou de causar a mudança. No Organizador com
milhares de entradas, isto atrapalha — lá o certo é nenhum stagger.

---

## 7. Fase 3 — Cards dinâmicos (resolve P4 e P12)

### 6.1 `Card` com variantes e estado

```slint
export enum CardKind { filled, elevated, outlined }

export component Card inherits Rectangle {
    in property <CardKind> kind: CardKind.filled;
    in property <bool> interactive: false;
    in property <bool> selected: false;

    callback clicked();

    background: kind == CardKind.filled   ? M3.surface_container_high
              : kind == CardKind.elevated ? M3.surface_container_low
              :                             M3.surface;
    border-radius: Shape.md;
    border-width: kind == CardKind.outlined ? 1px : 0px;
    border-color: selected ? M3.primary : M3.outline_variant;

    drop-shadow-color: #00000080;

    states [
        pressed_s when interactive && ta.pressed : {
            root.drop-shadow-blur: Elevation.l1;
            root.drop-shadow-offset-y: 1px;
        }
        hover_s when interactive && ta.has-hover : {
            root.drop-shadow-blur: Elevation.l3;
            root.drop-shadow-offset-y: 6px;
        }
        rest_s when true : {
            root.drop-shadow-blur: kind == CardKind.elevated ? Elevation.l1 : Elevation.l0;
            root.drop-shadow-offset-y: kind == CardKind.elevated ? 1px : 0px;
        }
    ]
    in  { animate drop-shadow-blur, drop-shadow-offset-y { duration: Motion.short4; easing: Motion.emphasized; } }
    out { animate drop-shadow-blur, drop-shadow-offset-y { duration: Motion.short2; easing: Motion.standard; } }

    ta := TouchArea { enabled: root.interactive; clicked => { root.clicked(); } }
    StateLayer { hovered: ta.has-hover; pressed: ta.pressed; }
}
```

### 6.2 `StatCard` — os 4 cards do dashboard

Hoje são 4 blocos quase idênticos de ~20 linhas cada (`dashboard.slint:67-139`), com `height: 110px`
travado e um "clique para ver a lista" que só aparece no hover — ou seja, o card **não avisa** que
é clicável até o mouse chegar. Vira um componente:

```slint
export component StatCard inherits Card {
    in property <string> label;
    in property <string> value;
    in property <string> hint;
    in property <color> accent;
    in property <image> icon;
    in property <bool> loading: false;
    in property <bool> has_details: false;

    kind: CardKind.filled;
    interactive: has_details;
    min-height: 116px;          // min-height, não height: cresce se o valor quebrar linha
    // ícone colorizado com `accent`, valor em Type.display_s, hint sempre visível quando
    // has_details (com opacidade 0.6 → 1.0 no hover), shimmer quando `loading`
}
```

Os 4 cards do dashboard passam a caber em 4 blocos de 8 linhas, e ganham:
affordance permanente, estado de carregamento, elevação no hover, ripple no clique.

### 6.3 Onde mais aplicar

| View | Card dinâmico |
|---|---|
| Installer | cada item da fila vira `Card` com badge de status (aguardando/instalando/ok/erro) e barra de progresso própria |
| Organizador | entradas viram cards selecionáveis (`selected: true` → borda `M3.primary` + `primary_container` a 12%) em vez de linhas |
| Merger | "Origem atual" e "Fila de tarefas" viram cards `elevated`; cada job, um card com progresso |
| Tray | itens com miniatura do Sim/Lote quando disponível |
| Config | cada seção já é card — só migrar para `CardKind.outlined` e agrupar os *chips* de tema/idioma |

### 6.4 Chips (tema, idioma, limite de tamanho)

Config usa botões para o que MD3 chama de **filter chip** (`config.slint`, seções Tema Visual /
Idioma / Limite). Um `FilterChip` com `selected` animando `background` e um check de entrada
representa melhor a semântica de "escolha exclusiva" — e o mesmo componente serve para
"Originais após unificar: Manter/Desativar/Backup/Excluir" no Merger.

---

## 8. Fase 4 — Ícones (resolve P1 e P2)

Emoji é a raiz dos dois piores defeitos visuais. Substituir por **Material Symbols em SVG**,
embarcados em `assets/icons/`:

- `Image { source: @image-url("../../assets/icons/home.svg"); colorize: M3.on_surface; }`
  — verificado: SVG + `colorize` compilam e permitem tingir o ícone com o token de cor,
  o que emoji nunca permitiu.
- `resvg` já está na árvore de dependências (`Cargo.lock`), então **não há custo novo de build**.
- Remover o emoji dos textos (`I18n.tr("🧹 Limpar Cache do Jogo")` → `I18n.tr("Limpar Cache do Jogo")`),
  o que também limpa os `assets/locales/*.json` e conserta P1.
- Ícones necessários (~24): home, package, folder, link, inbox, language, palette, settings,
  refresh, broom, shield, save, search, plus, edit, trash, copy, move, check, close, warning,
  info, download, play.

Manter `💎` do logo como imagem própria (`assets/s4suite.png` já existe) em vez de emoji.

---

## 9. Fase 5 — Layout responsivo (resolve P3)

A janela abriu em 934px e três views já vazaram conteúdo. Correções:

1. **Toolbars em `Flow`**: o Organizador tem 11 botões em duas `HorizontalLayout` fixas. Migrar
   para um contêiner que quebra linha quando `root.width` cai abaixo de um limiar, ou envolver em
   `ScrollView { viewport-height: 0; }` horizontal.
2. **Ações secundárias em overflow**: acima de 6 ações, as 3 menos usadas ("Ver Desativados",
   "Navegar…", "Limpar Lixo") vão para um `PopupWindow` de menu "⋮" — padrão MD3 de top app bar.
3. **`min-width` nos botões** e `overflow: elide` nos rótulos, para degradar em vez de vazar.
4. **Breakpoint da navegação**: abaixo de ~900px de largura, a `NavigationRail` colapsa para
   72px (só ícones, com tooltip). Acima, 240px com rótulo. Animar a largura com
   `animate width { duration: Motion.medium2; easing: Motion.emphasized; }` — a barra lateral
   deslizando é uma das transições mais visíveis do MD3.
5. **`preferred-width` da janela**: 1150px hoje (`main.slint:17`); adicionar `min-width: 880px` e
   `min-height: 620px` para o layout nunca entrar em território não testado.

---

## 10. Verificações já feitas neste repositório

Antes de escrever este plano, os construtos abaixo foram compilados contra `slint 1.17.1` neste
próprio projeto (probes adicionados a `ui/palette.slint`, build limpo, e revertidos em seguida):

| Construto | Status |
|---|---|
| `property <easing>`, `property <duration>`, `property <length>` em `global` | ✅ compila |
| `cubic-bezier(...)` como valor de `easing` | ✅ |
| `states [ ... ]` com blocos `in { animate ... }` / `out { animate ... }` | ✅ |
| `drop-shadow-blur` / `-color` / `-offset-y` animados | ✅ |
| `Timer { interval; running; triggered => }` | ✅ |
| `TouchArea.pressed-x` / `.pressed-y` / `.pressed` | ✅ |
| `clip: true` para ripple | ✅ (mas recorta o drop-shadow do mesmo elemento — usar wrapper) |
| `Image { source: @image-url("*.svg"); colorize: ... }` | ✅ |
| `@linear-gradient(...)` como `background` | ✅ |

Ou seja: **nada neste plano depende de recurso que o Slint deste projeto não tenha.**

E do lado da Hydra Shell, conferido na instalação ativa desta máquina:

| Fato | Onde foi conferido |
|---|---|
| `~/.config/noctalia/colors.json` existe e traz 16 chaves `mXxx` | arquivo lido, esquema verde ativo |
| Registro de app segue `{id, name, category, input, outputs}` | `Services/Theming/TemplateRegistry.qml:91-101` (entrada do Translator) |
| Templates ficam em `Assets/Templates/<nome>.json` | 33 templates presentes, incluindo `sims4-mod-translator.json` |
| Sintaxe `{{colors.<role>.<mode>.hex}}`, modos `default`/`dark`/`light` | `Scripts/python/src/theming/lib/renderer.py:1-20,144` |
| `default` = modo ativo, via `--default-mode` | `Services/Theming/TemplateProcessor.qml:163,453,478,500` |
| Conjunto MD3 completo é gerado (containers, `surface_dim/bright`, `outline_variant`, `scrim`, `*_fixed`) | `Scripts/python/src/theming/lib/material.py:159-235` |
| O `theme.json` do Translator é escrito e está atualizado | `~/.config/sims4-mod-translator/theme.json` lido, coerente com `colors.json` |

---

## 11. Ordem de execução

Cada etapa é um commit que deixa o app rodando. Só as etapas 2 e 3 tocam em `src/`, e nenhuma
altera a fiação UI↔engine (propriedades e callbacks de `MainWindow` ficam intactos).

| # | Etapa | Arquivos | Risco | Retorno visual |
|---|---|---|---|---|
| 1 | Tokens (`M3` **`in-out`**, `Motion`, `Type`, `Shape`, `Elevation`) + alias `Theme` | `ui/theme/*.slint`, `ui/palette.slint` | baixo | nenhum ainda |
| 2 | `src/core/theme.rs`: `Roles`, tabela dos 4 temas embutidos, `apply()` | `src/core/theme.rs`, `src/core/mod.rs`, `src/bridge/slint_adapter.rs` | baixo | nenhum (paridade com hoje) |
| 3 | Cascata da Hydra Shell + `spawn_watcher` + 5º chip “Sistema” | `src/core/theme.rs`, `src/core/config.rs`, `ui/views/config.slint` | médio | **alto** (§4) |
| 4 | Template `s4suite.json` + registro na shell | **repo hydra-shell** | baixo | **alto** (28 papéis nativos) |
| 5 | Ícones SVG + remoção dos emoji dos textos e locales | `assets/icons/`, `ui/**`, `assets/locales/*.json` | baixo | **alto** (mata P1 e P2) |
| 6 | `StateLayer`, `Ripple`, família de botões | `ui/components/button.slint` | baixo | **alto** (P7) |
| 7 | `NavigationRail` com indicador deslizante | `ui/components/sidebar.slint` | baixo | **alto** |
| 8 | `Card`/`StatCard`/`FilterChip` + migração do dashboard | `ui/components/card.slint`, `ui/views/dashboard.slint` | médio | **alto** (P4, P12) |
| 9 | `Scrim`/`Dialog` + migração dos 7 overlays | `ui/components/dialog.slint`, todas as views | médio | alto (P6) |
| 10 | `ViewSlot` + crossfade de abas | `ui/main.slint` | **médio-alto** (P5 — mexe no ciclo de vida das views) | alto |
| 11 | `StatusBar` compartilhada | `ui/components/status_bar.slint` + 6 views | baixo | baixo (mas −150 linhas) |
| 12 | Toolbars responsivas, overflow, breakpoint da rail | `organizer.slint`, `tray.slint`, `main.slint` | médio | alto (P3) |
| 13 | Foco de teclado + `accessible-*` | todos os componentes | médio | nenhum visual, alto em usabilidade (P10) |

**Por que 2–4 vêm tão cedo:** a decisão “tokens são `in-out`, resolvidos no Rust” (§3.1) precisa
valer desde o primeiro componente escrito. Fazer os componentes primeiro e a integração depois
significaria reescrever todo `.slint` que tivesse ternário de tema. As etapas 2 e 3 não mudam nada
na tela — são a fundação que faz o resto não precisar de retrabalho.

**Etapa 4 é no outro repositório** (`/home/raell/Projetos/hydra-shell`) e é independente: a etapa 3
já funciona pelo nível 2 (`~/.config/noctalia/colors.json`) enquanto o template não existir.

**Etapa 10 é a única que merece cautela.** Trocar `if` por `ViewSlot` mantém todas as views vivas
simultaneamente; se algum `init =>` de view tiver efeito colateral (disparar callback para o
motor), ele passará a rodar no arranque em vez de na primeira visita. Auditar os `init` das 8 views
antes, e rodar `tests/bridge_wiring.rs` logo depois — é exatamente a classe de bug que já mordeu
este projeto antes.

## 12. Como validar cada etapa

```bash
cargo build && cargo test          # fiação UI↔engine intacta
./target/debug/s4suite             # inspeção visual das 8 abas
```

Para as etapas 2–4 (tema do sistema), o teste que importa é ao vivo: com o app aberto, trocar o
wallpaper ou o esquema na Hydra Shell e conferir que as cores escorrem em ≤1s (700ms de polling +
300ms de animação) sem piscar e sem sobrar cor do tema anterior. Alternar dark↔light **duas vezes**
— é o ciclo que expõe aplicação parcial de papéis (§4.4).

Sugestão: repetir a captura das 8 abas a cada etapa e comparar lado a lado com os prints de
referência tirados hoje (`v-instalador.png`, `v-organizador.png`, `v-merger.png`, `v-tray.png`,
`v-traducoes.png`, `v-config.png`, `s4suite-dashboard.png`). Regressão de layout em Slint é fácil
de introduzir e difícil de perceber lendo diff.

---

## 13. O que a execução mudou no plano

Registrado depois de implementar as etapas 1–11. Nada aqui é hipótese: são coisas que só
apareceram ao compilar e rodar.

### 13.1 Correções ao plano

| Ponto | O plano dizia | O que valeu na prática |
|---|---|---|
| **Ícones** | 24 SVGs de Material Symbols em `assets/icons/` | **Fonte Tabler Icons**, a mesma que a Hydra Shell usa (`Assets/Fonts/tabler/noctalia-tabler-icons.ttf`). Um `import "…ttf";` no `.slint` registra a família — `slint::register_font_from_memory` **não existe** em 1.17, é método de trait de renderer. Bônus: o app passa a usar o vocabulário visual do próprio desktop |
| **Severidade do status** | Adicionar propriedades novas por vista | Um global `StatusRules` com dois callbacks puros implementados no Rust (`core::status`), no mesmo padrão do `I18n`. Zero mudança nas 77 chamadas de `set_*_status`. Slint não tem `starts-with` nem substring, então isso **não** dá para resolver no `.slint` |
| **Breakpoint da rail** | `collapsed: root.width < 900px` dentro do `HorizontalLayout` | Fecha um **ciclo de binding** (`layoutinfo-h → collapsed → width → layoutinfo-h`) que o compilador acusa e que pode dar panic. A rail saiu do layout e é posicionada à mão |
| **Transições de estado** | `states [ … ]` seguido de `in { … } out { … }` | Os blocos `in`/`out` moram **dentro** de cada estado, não depois do array |
| **`transparent`** | comparável como qualquer cor | É aceito como valor mas não como operando: `swatch != transparent` não compila. Comparar com `#00000000` |
| **Crossfade** | Manter as 8 vistas vivas (Opção A) | Só **duas**: a atual e a anterior (`prev_tab`). Suficiente para a animação e evita construir as listas do Organizador com a aba fechada |

### 13.2 O que o plano não previa

- **Auditoria dos `init =>`** (a cautela da etapa 10): não existe nenhum nas 8 vistas. O risco que
  o plano levantava simplesmente não se materializou.
- **`Card` precisou expor `hovered`**: um `StatCard` que declarasse a própria `TouchArea` roubaria
  os eventos da do card.
- **`StatCard` precisa de `vertical-stretch: 0`**: herdado de `Rectangle`, o card engolia toda a
  folga vertical da tela.
- **Bug pego por teste**: `Roles::builtin(99)` saturava no *último* tema em vez de cair no
  primeiro, divergindo de `builtin_name_from_index` — o nome gravado no config não bateria com a
  cor aplicada.
- **Bug pego por teste**: um template parcial derivava `surface_bright` de uma âncora mais baixa
  que o `surface_container_high` vindo pronto do arquivo, invertendo a rampa. A derivação virou
  encadeada, nível a nível.

### 13.3 O que ficou de fora

- **Etapa 12 (layout responsivo)**: a rail já colapsa por breakpoint e a janela tem `min-width`,
  mas as barras de ferramentas do Organizador (11 ações) e do Tray ainda **elidem** os rótulos a
  934px. Falta o menu de overflow "⋮" descrito em §9.2.
- **Etapa 13 (foco de teclado)**: `Button`, `FilterChip` e `NavItem` já têm `FocusScope` e
  respondem a Espaço/Enter. Falta `accessible-*` e ordem de tabulação explícita.
- **Etapa 9 (diálogos)**: `Scrim` e `Dialog` **não** foram criados. Os 7 overlays continuam como
  `Rectangle` cru, aparecendo sem transição.
- **Template na shell**: `Assets/Templates/s4suite.json` e a entrada no `TemplateRegistry.qml`
  estão criados e o render foi validado à mão, mas o template ainda precisa ser **habilitado** em
  Settings → Color Scheme → Templates da Hydra Shell para regenerar sozinho a cada troca de tema.
