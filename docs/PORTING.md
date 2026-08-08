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
| `s4translator.py` | `core/i18n.rs` | ⚠️ portado, não conectado à UI |
| `s4theme.py` | `ui/palette.slint` | ⚠️ tema não persiste |
| `s4installer.py` | `engine/installer.rs` | ✅ fluxo completo e testado |
| `s4merger.py` | `engine/merger.rs` + `engine/dbpf.rs` | ✅ merge real + pós-merge |
| `s4tray.py` | `engine/tray.rs` | ⚠️ engine ok, bridge é no-op |
| `s4reshade.py` | `engine/reshade.rs` | ⚠️ quase completo |
| `s4disabled.py` | `engine/disabled.rs` | ✅ ligado ao organizador |
| `ui/*_tab.py` | `ui/views/*.slint` | ⚠️ layout pronto, strings hardcoded |

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

### Etapa 4 — Tray

- [ ] `start_tray_import` deve chamar `analyze_tray_source()` e
      `import_tray_item()`.
- [ ] Preencher tipo e contagem de arquivos a partir da análise real
      (hoje é `"Sim / Lote"` e `"1"` hardcoded).

### Etapa 5 — i18n e configurações

- [ ] Expor `language` como property da `MainWindow` e trocar as strings
      hardcoded das views por lookup traduzido.
- [ ] Persistir tema e `merge_max_size` no config (`save_config` hoje é
      um load-and-save sem efeito).
- [ ] Reconciliar `assets/locales/*.json` com `legacy/locales/*.json`, que têm
      bem mais chaves (en: 25 KB no legado vs 15 KB no atual; falta `pt.json`
      quase inteiro, com apenas 82 bytes).

### Etapa 6 — Reshade e acabamento

- [ ] Setar `is_reshade_installed` a partir do estado real em disco.
- [ ] Mover o download HTTP do `install_reshade` para `spawn_blocking`
      (hoje roda em `spawn_local` e trava a UI).

### Etapa 7 — Testes e empacotamento

- [x] `tests/installer_flow.rs`, `tests/organizer_flow.rs`,
      `tests/merger_flow.rs` — 30 testes de integração, 35 no total com os
      unitários.
- [ ] Cobrir `tray` e `reshade` quando as etapas 4 e 6 fecharem.
- [ ] Substituir o AppImage do PyInstaller por empacotamento do binário Rust.
