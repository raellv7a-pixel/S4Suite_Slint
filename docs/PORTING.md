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
| `ui/translations_tab.py` | `engine/translations.rs` | ✅ usa o instalador real |
| `ui/dashboard_tab.py` | `engine/stats.rs` | ✅ varredura única e detalhes |
| `ui/*_tab.py` | `ui/views/*.slint` | ✅ paridade de features |
| `build.sh` (PyInstaller) | `build.sh` (AppImage do binário) | ✅ portado |

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

O fluxo corrente usa exclusivamente a fila. Os callbacks órfãos `start_merge`
e `post_merge_action` foram removidos: a antiga pós-ação reconstruía a seleção
atual, em vez de usar os arquivos efetivamente revisados e processados.

- [x] Origem, destino e lista de packages persistidos no `AppState`.
- [x] Merge DBPF real em `spawn_blocking`, com barra de progresso alimentada
      pelo callback do engine.
- [x] `MergeReview::prepare()` analisa as entradas de cada tarefa em background:
      tokens conservadores, tuning/XML e scripts no diretório imediato são
      indícios apresentados com motivos, não provas de incompatibilidade.
- [x] `approve()` produz `AuthorizedMergeJob`, não clonável e vinculado ao job,
      caminhos, identidades e SHA-256 das entradas/scripts. `run_merge_queue()`
      aceita somente esses tokens e revalida antes do merge e da pós-ação.
- [x] Delete exige `EXCLUIR ORIGINAIS` literalmente para cada tarefa. O diálogo
      limpa a frase e troca o nonce por tarefa; eventos de outra revisão são
      recusados. Mudanças na fila, seleção, destino ou limite invalidam a revisão.
- [x] Keep / Disable / Backup / Delete preservados; a pós-ação interna nunca
      roda após merge parcial, malsucedido ou com fontes/destino alterados.
- [x] Falhas de abertura/leitura e payload ausente/truncado/zlib inválido entram
      no relatório, sem modificar a precedência TGI nem recomprimir recursos.
- [x] Backup dos originais usa streaming, verificação de conteúdo e temporário
      sem sobrescrita; só remove originais após publicação válida.
- [x] Regressões em `tests/merger_flow.rs` e `tests/bridge_wiring.rs`.

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

## Paridade com o app PyQt

Comparação linha a linha entre `legacy/ui/*.py` e `ui/views/*.slint`.

### Etapa 7 — Engine de traduções ✅ concluída

`on_select_translation_file` fazia um `fs::copy` cru: um `.zip` de tradução era
copiado **como zip** para dentro de `Mods`, onde o jogo não lê nada, e qualquer
erro sumia num `let _ =`. A exclusão concatenava o nome vindo da UI direto no
caminho, sem passar pela camada de segurança.

- [x] `engine/translations.rs` reaproveita o instalador, como o app PyQt já
      fazia (`translations_tab.py:166` chama o mesmo `install_mod` apontando
      para `01_Traducoes`). Vem de graça a varredura de executáveis, a detecção
      de cópia já instalada, o backup do substituído e o rollback.
- [x] `StagedFile::destination` e `execute_installation` passaram a receber a
      base gerenciada em vez de assumir `00_Triagem_Novos` — é o que permite o
      reaproveitamento.
- [x] A detecção de conflito varre `Mods` inteiro, mas só o que é **novo**
      aterrissa em `01_Traducoes`: uma tradução guardada fora da pasta é
      atualizada no lugar, em vez de virar uma segunda cópia carregando no jogo.
- [x] `prepare_translation_removal()` resolve uma entrada exata e captura raízes,
      identidade e árvore do item sem escrever no disco. A UI guarda essa
      operação e apresenta confirmação permanente com nome/cancelar/excluir.
- [x] Confirmar revalida configuração, raízes e conteúdo; cancelamento ou alvo
      alterado não apagam nada. `remove_translation()` permanece para consumidores
      imediatos de engine, com o escopo adicional validado.
- [x] Regressões em `tests/translations_flow.rs` e `tests/bridge_wiring.rs`.

### Etapa 8 — Organizador completo ✅ concluída

Era a maior lacuna: 1112 linhas em `organizer_tab.py` contra 211 no `.slint`.

- [x] `create_folder`, `rename_item`, `transfer_items` (mover/copiar) e
      `remove_entries`, todos passando pela validação de escopo. A raiz de
      `Mods` é protegida de renomear e apagar.
- [x] `sanitize_entry_name` usa a lista de caracteres proibidos do **Windows**:
      a pasta `Mods` costuma ser compartilhada com instalações Windows por
      drive comum.
- [x] Mover pasta para dentro de si mesma é recusado — o `rename` falha, mas a
      cópia recursiva entraria em laço criando cópias dentro de cópias.
- [x] `move_entry` cai para copiar-e-apagar quando `fs::rename` recusa: `Mods`
      costuma estar num disco separado, onde rename entre sistemas de arquivos
      não funciona.
- [x] Aviso de profundidade de script após transferir. O jogo só carrega
      `.ts4script` até um nível de subpasta, e mover um mod para
      `Mods/Categoria/Autor/` o desliga sem o jogo reclamar de nada.
- [x] `filter_tree` traz junto os ancestrais de cada acerto: a árvore é plana no
      modelo mas desenhada aninhada, e só as linhas que casam deixariam os
      resultados órfãos.
- [x] Painel de desativados com restaurar e excluir em lote. `list_disabled` tem
      o **disco** como fonte da verdade, não o manifesto: um `.disabled`
      renomeado à mão fora do app precisa aparecer, senão fica invisível e sem
      como ser reativado.
- [x] Seleção múltipla na árvore, com pastas incluídas — mover, copiar,
      renomear e excluir valem para elas.
- [x] 28 testes em `tests/organizer_flow.rs`.

`remove_entries` trata pastas; `remove_files` continua recusando-as, porque na
limpeza de lixo e nas duplicatas uma pasta na lista só poderia ser engano.

### Etapa 9 — Tray e merger com paridade ✅ concluída

- [x] Tray: destino de CC por item, aplicar-a-todos e pular, com `cc_dir()`
      resolvendo o padrão. Recusa pasta fora de `Mods` — lá o jogo não carrega
      o CC, então copiar seria só gastar disco.
- [x] Itens pulados continuam à vista, em cinza, para o usuário voltar atrás.
      Os binários de Tray nunca seguem o destino do CC: só funcionam em `Tray/`.
- [x] Merger: `run_merge_queue` executa a fila em ordem, com callbacks de status
      e progresso identificados pelo índice do job — sem o índice, a barra da UI
      não saberia de qual tarefa é o avanço.
- [x] A pós-ação só roda com merge **completo**: num merge parcial, apagar ou
      desativar os originais perderia o conteúdo que não entrou.
- [x] Uma tarefa que falha não interrompe as seguintes — a fila é justamente
      onde o usuário deixa vários grupos rodando sem olhar.
- [x] 12 testes de tray e 15 de merger.

### Etapa 10 — Installer e dashboard ✅ concluída

- [x] Smart Mod Installer V4: `detect_mutually_exclusive` retorna grupos de
      **possíveis** variantes por fonte e pasta interna. Apenas marcadores
      explícitos (`Options`, `Exclusive`, `Choose One`, `Pick One`, `Select One`,
      `Only One`, com espaços, `_` ou `-`) acionam a sugestão. Nome do ZIP,
      nome do arquivo e substrings como `Optional` não bastam; a heurística
      não comprova incompatibilidade.
- [x] Cada grupo oferece **Instalar todos** (padrão), **Escolher apenas um** ou
      **Seleção manual de múltiplos arquivos**. Fontes homônimas recebem raízes
      de staging distintas. Seleções vazias não avançam; arquivos comuns e
      outros grupos permanecem intactos.
- [x] Após resolver todos os grupos, a fila e as contagens refletem somente os
      arquivos selecionados. Dependências são lidas em background e filtradas
      pela seleção; falhas de preparação continuam no resumo. A confirmação
      final permanece obrigatória antes de gravar, sem alterar segurança,
      backups ou rollback do instalador.
- [x] `engine/stats.rs` faz uma varredura única em vez de três, e passou a usar
      `walk_user_mods` — staging e backups são cópias do que já está contado, e
      incluí-los mostrava quase o dobro do espaço ocupado.
- [x] Cards de scripts, tamanho e tray abrem a lista por trás do número.
- [x] 25 testes de installer e 6 de stats. Regressões do V4 cobrem roupas,
      múltiplos ZIPs, fontes homônimas, escolhas independentes, seleção manual
      e atualização com backup. Smoke gráfico com arquivos sintéticos confirmou
      os três modos, o resumo prévio, os payloads instalados e a limpeza do staging.

### Etapa 11 — Validação de todos os motores ✅ concluída

**Critério de conclusão do port.** Três camadas: engine, bridge e manual.

- [x] Suíte por engine — 132 testes, todos verdes.
- [x] `tests/bridge_wiring.rs`: todo callback tem handler dos dois lados, e
      acioná-lo muda o disco de verdade, com a janela montada num backend
      headless. Existe por causa da classe de bug que apareceu três vezes neste
      port — `on_start_merge` simulado, `check_scripts` desconectado,
      `start_tray_import` no-op — sempre com o engine correto por trás.
- [x] O teste de fiação pegou **dois botões mortos** ao ser escrito: o
      "Selecionar Arquivos de Mods" não estava ligado no `main.slint` e, sendo o
      único jeito de encher a fila, deixava o instalador inteiro inutilizável
      pela UI; o "Atualizar" do painel também não chamava nada.
- [x] `dbpf_flow.rs`: 11 testes conferindo **bytes**, não só `Ok`. Um erro de
      offset não dá exceção — grava um arquivo que o jogo abre e lê errado.
      Achou a variante `IndexOutOfBounds` que existia e nunca era retornada.
- [x] `docs/VALIDACAO.md` com o roteiro manual contra a pasta `Mods` real.

### Etapa 12 — Empacotamento ✅ concluída

- [x] `build.sh` gera o AppImage a partir do binário Rust. O do PyInstaller
      precisava de venv, do bundle inteiro do Python e de apagar à mão as libs
      do sistema duplicadas; aqui o binário já carrega os locales embutidos por
      `rust-embed` e linka contra a libc do sistema.
- [x] Perfil de release com `lto`, `codegen-units = 1` e `strip`: 49 MB → 31 MB
      de binário, 18 MB de AppImage. `panic = "abort"` fica de fora de
      propósito — as tarefas em `spawn_blocking` isolam falhas por arquivo, e
      sem unwind um package corrompido derrubaria o app inteiro.

## Estabilização P0/P1 — arquivos e manutenção

- `core::safety` protege todas as raízes autorizadas, inclusive Mods e raízes
  sobrepostas, e exige caminhos existentes reais. Canonicalização permissiva
  permanece somente para usos não destrutivos. Links/reparse no alvo ou ancestrais,
  caminhos vazios, tipo incorreto e escopo externo são recusados. Subpastas e
  arquivos internos continuam removíveis; links filhos de uma pasta real são
  desvinculados sem seguir seus destinos.
- Os consumidores de remoção foram revistos. ReShade não restaura uma DLL sobre
  um alvo cuja exclusão foi recusada e propaga falhas de remoção/restauração.
- `engine::cache::clean_game_cache()` exige uma raiz de jogo real com Mods real.
  Só remove os arquivos `localthumbcache.package`/`spotlight_thumbnails.package`
  e diretórios `cache`/`cachestr`. Retorna removidos, ausentes e falhas independentes;
  o dashboard mostra as contagens e os detalhes de erros sem esconder sucesso parcial.
- `engine::backup::create_zip_archive()` preserva estrutura/diretórios vazios e
  copia em fluxo. Erros de percurso/leitura/escrita, mudanças na origem ou ZIP
  inválido abortam. Usa temporário no destino, finish/flush/sync, releitura com CRC
  e `persist_noclobber`; destino existente é preservado e só o temporário próprio
  é removido em falha. Destino dentro da origem e links são recusados.
- Cache, backup e merger compartilham uma permissão RAII de processamento em
  background para impedir operações conflitantes; navegação permanece disponível.
- Novas mensagens e diálogos seguem MD3 e PT/EN/ES. Regressões usam somente
  diretórios temporários e dados sintéticos em `safety_flow`, `cache_flow`,
  `backup_flow`, `translations_flow`, `merger_flow` e testes do bridge.

**Limite de segurança:** validação por caminhos com `std::fs` e fingerprints
repetidos não equivale a uma transação atômica contra substituição hostil de
ancestrais entre syscalls. Não altere a árvore externamente durante as operações;
faça backup dos saves com o jogo fechado. Isolamento forte exigiria snapshots
ou operações relativas a descritores/handles, sem seguir links.

## O que falta

- **ReShade** — adiado por decisão do usuário; precisa de mais trabalho de
  implementação. Ver a etapa 6 e as pendências em `docs/VALIDACAO.md`.
- **Validação manual** — o roteiro de `docs/VALIDACAO.md` contra um `Mods` real
  ainda não foi percorrido.
