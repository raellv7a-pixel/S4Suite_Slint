# Port S4Suite: PyQt → Rust + Slint

Documento de acompanhamento da migração. Atualize o status conforme cada etapa
fechar.

## Arquitetura alvo

```
src/
  core/      # infra sem UI: config, i18n, safety (validação de paths)
  engine/    # regra de negócio pura, sem Slint: dbpf, installer, merger,
             # organizer, tray, reshade, disabled
  bridge/    # única camada que conhece Slint: adapter de callbacks + models
ui/          # Slint: main.slint, components/, views/
assets/      # locales/ (embutidos via rust-embed) e ícone
legacy/      # código PyQt original — referência de comportamento, não compila
```

**Regra de ouro:** `engine/` e `core/` nunca importam `slint`. Toda conversão
para `SharedString`/`ModelRc` acontece em `bridge/`. Isso mantém o engine
testável sem event loop.

## Correspondência Python → Rust

| Python (legacy) | Rust | Status |
|---|---|---|
| `s4common.py` | `core/config.rs` + `core/safety.rs` | ✅ portado |
| `s4translator.py` | `core/i18n.rs` + `ui/i18n.slint` | ✅ ligado à UI |
| `s4theme.py` | `ui/palette.slint` | ✅ tema persiste no config |
| `s4installer.py` | `engine/installer.rs` | ✅ fluxo completo e testado |
| `s4merger.py` | `engine/merger.rs` + `engine/dbpf.rs` | ✅ merge real + pós-merge |
| `s4tray.py` | `engine/tray.rs` | ✅ importação real e testada |
| `s4reshade.py` | `engine/reshade.rs` | ⚠️ parcial, retomada adiada |
| `s4disabled.py` | `engine/disabled.rs` | ✅ ligado ao organizador |
| `ui/translations_tab.py` | `ui/views/translations.slint` | ⚠️ sem engine, só copia |
| `ui/*_tab.py` | `ui/views/*.slint` | ⚠️ traduzidas; paridade de features pendente |

## Sequência de trabalho

As etapas estão ordenadas por dependência e por risco. Cada uma deve terminar
com `cargo check` verde e o fluxo exercitável na UI.

### Etapa 1 — Installer ✅ concluída

O elo que faltava: `on_start_install` analisava `.s4suite_staging`, mas nada
nunca extraía arquivos para lá. `extract_archive_to_staging()` e
`scan_archive_security()` existiam no engine sem nenhum chamador.

- [x] `bridge/state.rs`: `AppState` guarda os caminhos reais dos arquivos
      selecionados e a análise pendente entre callbacks.
- [x] `engine::installer::prepare_staging()`: extrai cada fonte sob sua
      identidade (`Meu Mod v2.1.zip` → `meu_mod_v2_1/`), aceita `.package`
      avulso e isola falhas por arquivo em vez de abortar o lote.
- [x] Fluxo em duas fases: analisar (staging) → confirmar → gravar em Mods.
      Ambas em `spawn_blocking`; erro real chega à UI em vez de travar a tela
      em "processando".
- [x] Staging é limpo em erro, cancelamento e limpeza de fila.
- [x] `take_pending_install()` torna o duplo-clique em "Prosseguir" inofensivo.
- [x] 12 testes de integração em `tests/installer_flow.rs`.

**Dois bugs de engine corrigidos aqui:**

1. `calculate_conflicts` varria `Mods` inteiro para montar o mapa de arquivos
   existentes — incluindo o próprio `.s4suite_staging`, que mora dentro de
   `Mods`. Cada arquivo recém-extraído se encontrava e era classificado como
   `ExactMatch`, então **nada era instalado**. Staging e backups agora são
   excluídos da varredura (teste de regressão:
   `staging_nao_e_confundido_com_mods_ja_instalados`).
2. Arquivos novos eram despejados na raiz de `Mods` com nome achatado.
   Agora seguem a convenção do app PyQt: `Mods/00_Triagem_Novos/<mod>/…`,
   com `.ts4script` mantido raso — o jogo só carrega scripts até um nível de
   subpasta.

**Decisões de comportamento:**

- Arquivos idênticos (`ExactMatch`) são pulados, não recopiados.
- Toda sobrescrita passa por `.s4suite_backups/`, e `execute_installation`
  reverte a operação inteira se qualquer cópia falhar.

**Ainda não portado do `s4installer.py`** (fica para um refinamento futuro):
`detect_mutually_exclusive` (diálogo de escolha entre variantes do mesmo mod)
e `detect_dependencies` — este último existe no engine mas ainda não é exibido
na UI.

### Etapa 2 — Organizador + mods desativados ✅ concluída

Feita antes do merger de propósito: a ação pós-merge "desativar originais"
depende do `DisabledManager`, então inverter a ordem evitaria um TODO.

- [x] `DisabledManager` instanciado no `AppState` e ligado ao organizador.
- [x] Árvore de mods clicável, com item selecionado e rolagem (uma pasta Mods
      real tem centenas de entradas; sem `ScrollView` o excedente era
      inalcançável).
- [x] `toggle_disabled_mod(path)` recebe o caminho e alterna de verdade —
      antes não recebia argumento e só recarregava a árvore.
- [x] Diálogo de confirmação genérico que **lista os arquivos** antes de
      apagar, usado por duplicatas e por limpeza de lixo.
- [x] `DuplicateGroup::keeper()` / `redundant()`: a cópia mais rasa é
      preservada, as demais entram na lista de remoção.
- [x] `organizer::remove_files()` centraliza as exclusões, sempre via
      `safe_remove_file`.
- [x] 8 testes em `tests/organizer_flow.rs`.

**Bugs corrigidos:**

1. **Mesma classe do bug do staging, agora no organizador.**
   `detect_duplicates`, `find_junk_files`, `check_script_depth` e
   `scan_mods_tree` varriam `Mods` inteiro, incluindo `.s4suite_staging` e
   `.s4suite_backups`. Como os backups são cópias byte-a-byte de mods
   instalados, todo mod atualizado aparecia como duplicata — e o usuário era
   convidado a apagar o próprio backup. Criado `engine::walk_user_mods()`,
   agora a única forma de percorrer a árvore de mods.
2. `clean_junk` apagava com `fs::remove_file` direto, sem passar pela camada
   de segurança e **sem confirmação**. Agora passa por `safe_remove_file` e
   exige confirmação com a lista à vista.
3. `check_scripts_clicked` nunca estava conectado no `main.slint`: o callback
   existia no Rust e o botão não chamava nada.

### Etapa 3 — Merger ✅ concluída

`on_start_merge` era uma simulação: dormia e abria o diálogo sem nunca chamar
`merge_sims4_packages()`. `post_merge_action` só trocava o texto do status —
"Arquivos originais apagados" aparecia sem que nada fosse apagado.

- [x] Origem, destino e lista de packages persistidos no `AppState`.
- [x] Merge DBPF real em `spawn_blocking`, com barra de progresso alimentada
      pelo callback do engine.
- [x] Botão de iniciar só habilita com origem e destino definidos.
- [x] `engine::merger::apply_post_merge_action()`: keep / disable / backup /
      delete implementados de verdade.
- [x] `common_ancestor()` define a raiz de segurança — nada fora da pasta de
      origem é apagado ou renomeado, mesmo que entre na lista.
- [x] Backup só remove os originais **depois** que o zip fecha com sucesso.
- [x] 10 testes em `tests/merger_flow.rs`.

A lógica pós-merge vive no `engine/`, não no `bridge/`: é regra de negócio e
precisa ser testável sem event loop (a regra de ouro no topo deste documento).

### Etapa 4 — Tray ✅ concluída

`start_tray_import` era um no-op decorativo: trocava o texto do status e nada
saía do lugar. Tipo e contagem vinham hardcoded como `"Sim / Lote"` e `"1"`.

- [x] Fluxo em duas fases igual ao installer: `prepare_tray_candidates()`
      extrai e analisa as fontes num diretório de trabalho
      (`TRAY_WORK_DIR`, fora de `Mods` e do `Tray`), depois `import_tray_item()`
      grava nos destinos reais. Ambas em `spawn_blocking`.
- [x] `TrayImportCandidate::kind_label()` / `total_files()` alimentam a lista
      com a classificação real (Sim, Lote, só CC).
- [x] Candidatos persistidos no `AppState`; `take_tray_candidates()` torna o
      duplo-clique inofensivo, como no installer.
- [x] Diretório de trabalho limpo a cada preparação (`clear_tray_work_dir`).
- [x] 10 testes em `tests/tray_flow.rs`.

`sanitize_import_name()` segue o `s4tray.py` — caracteres proibidos são
removidos, não substituídos — mas colapsa os espaços que a remoção deixa para
trás, senão `"Maria / Silva"` viraria a pasta `"Maria  Silva"`.

### Etapa 5 — i18n e configurações ✅ concluída

- [x] `ui/i18n.slint`: global `I18n` com `tr(key)`, que lê `lang` antes de
      chamar o callback Rust. Ler a property dentro da função é o que registra
      a dependência — sem isso, trocar de idioma não reavaliaria binding algum
      e a tela só mudaria depois de um reload.
- [x] Chave de tradução é o próprio texto em português, mesma convenção do
      PyQt: os locales do legado são reaproveitados sem reescrita, e o
      português dispensa arquivo (`pt.json` fica mínimo por design).
- [x] Todas as views passaram a usar `I18n.tr(...)`; `en.json` e `es.json`
      reconciliados com o legado (382 e 383 chaves).
- [x] Tema, idioma e `merge_limit_gb` persistidos de verdade no config e
      reaplicados na abertura por `apply_config_to_ui()`.
- [x] 7 testes em `tests/i18n_coverage.rs`, incluindo um que varre os `.slint`
      atrás de `I18n.tr("…")` e falha se alguma chave não existir nos locales —
      é o que impede uma string nova de aparecer em português no meio de uma
      interface em inglês.

### Etapa 6 — ReShade ⚠️ parcial (retomada adiada)

- [x] `detect_installation()` monta o `ReshadeStatus` a partir do disco:
      DLL injetada, manifesto e contagem de presets.
- [x] Download HTTP movido para `spawn_blocking` — em `spawn_local` ele rodava
      na thread da UI e congelava a janela inteira.
- [x] 8 testes em `tests/reshade_flow.rs`.
- [ ] **Adiado por decisão do usuário:** o módulo precisa de mais trabalho de
      implementação e será retomado depois da paridade dos demais. Inclui
      revisar o `legacy/reshade.exe` versionado no repositório.

## Paridade restante

Comparação linha a linha entre `legacy/ui/*.py` e `ui/views/*.slint`. O port é
funcional, mas estas peças do app PyQt ainda não existem no Rust/Slint.

### Etapa 7 — Engine de traduções

Hoje `on_select_translation_file` faz um `fs::copy` cru. O legado chama
`install_mod()`: extrai `.zip`/`.7z`, valida o `.package` e reporta o erro.

- [ ] `engine/translations.rs` portando `install_mod`.
- [ ] Bridge em `spawn_blocking`, com falha visível na UI em vez de `let _ =`.
- [ ] `tests/translations_flow.rs`.

### Etapa 8 — Organizador completo

A maior lacuna: `organizer_tab.py` tem 1112 linhas contra 211 do `.slint`.

- [ ] Gerenciamento de arquivos: nova pasta, mover, copiar, renomear, excluir —
      tudo through `core::safety`, nunca `fs::` direto.
- [ ] Busca/filtro na árvore de mods.
- [ ] Painel de desativados: restaurar e excluir em lote.
- [ ] Auto-fix de profundidade de script (`check_script_depth` já detecta, mas
      não corrige pela UI).

### Etapa 9 — Tray e merger com paridade

- [ ] Tray: pasta de destino por item, aplicar-a-todos, pular pacotes.
- [ ] Merger: fila de jobs com destino editável por job (hoje é um merge por
      vez).

### Etapa 10 — Installer e dashboard: acabamento

- [ ] `detect_mutually_exclusive` (diálogo de escolha entre variantes do mesmo
      mod) e `detect_dependencies` — o segundo existe no engine e não é exibido.
- [ ] Cards clicáveis do dashboard com diálogos de detalhe: tamanho por pasta,
      scripts, tray.

### Etapa 11 — Validação de todos os motores

**Critério de conclusão do port.** O projeto só é dado como concluído quando
todo motor passar aqui.

- [x] Suíte por engine: installer (12), organizer (8), merger (10), tray (10),
      reshade (8), i18n (7) + 5 unitários — 60 testes verdes.
- [ ] Cobrir `disabled`, `dbpf` e `translations` com suíte própria.
- [ ] Um teste headless de bridge por motor, garantindo que o callback Slint
      chama o engine de verdade. É a classe de bug que já apareceu três vezes
      neste port: `on_start_merge` simulado, `check_scripts` desconectado e
      `start_tray_import` no-op — todos com engine correto e testado por trás.
- [ ] Validação manual com a pasta `Mods` real, motor por motor, registrada em
      `docs/VALIDACAO.md`.

### Etapa 12 — Empacotamento

- [ ] Substituir o AppImage do PyInstaller por empacotamento do binário Rust
      (`cargo build --release`, `.desktop`, ícone).
