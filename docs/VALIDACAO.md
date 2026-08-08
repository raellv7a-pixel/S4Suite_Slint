# Validação dos motores — Rust + Slint

O port só é dado como concluído quando **todo motor** passar nas três camadas
abaixo. Testes automatizados provam que o engine faz a coisa certa e que o botão
chega até ele; nenhum deles prova que o resultado agrada num `Mods` real de
verdade, com centenas de mods e nomes acentuados.

## Camadas

1. **Engine** — suíte de integração por motor, sem event loop (`tests/*_flow.rs`).
2. **Bridge** — `tests/bridge_wiring.rs`: todo callback tem handler dos dois
   lados, e acioná-lo muda o disco.
3. **Manual** — este documento, contra a pasta `Mods` real.

## Estado automatizado

`cargo test` — **132 testes, todos verdes.**

| Motor | Arquivo | Testes | Camada 1 | Camada 2 |
|---|---|---:|:---:|:---:|
| Instalador | `installer_flow.rs` | 17 | ✅ | ✅ |
| Organizador | `organizer_flow.rs` | 28 | ✅ | ✅ |
| Merger | `merger_flow.rs` | 15 | ✅ | ✅ |
| Tray | `tray_flow.rs` | 12 | ✅ | ✅ |
| Traduções | `translations_flow.rs` | 12 | ✅ | — |
| DBPF | `dbpf_flow.rs` | 11 | ✅ | n/a |
| Estatísticas | `stats_flow.rs` | 6 | ✅ | ✅ |
| ReShade | `reshade_flow.rs` | 8 | ⚠️ parcial | — |
| i18n | `i18n_coverage.rs` | 7 | ✅ | ✅ |
| Bridge | `bridge_wiring.rs` | 11 | n/a | ✅ |
| Unitários | `src/engine/*` | 5 | ✅ | n/a |

Mods desativados (`engine/disabled.rs`) são cobertos dentro de
`organizer_flow.rs`, que é onde o motor é usado.

## Camada 3 — roteiro manual

Rode `cargo run` com a pasta do jogo configurada. Marque cada item e anote o
resultado. **Faça um backup de `Mods` antes**: o roteiro apaga arquivos de
propósito.

### Preparação

- [ ] `Mods` real configurado em Configurações, ou detectado sozinho.
- [ ] Painel abre e mostra contagens compatíveis com o que há em disco.

### Instalador

- [ ] Selecionar um `.zip` de mod → a fila mostra os arquivos que ele contém.
- [ ] Um zip com `.exe` dentro é recusado, e o staging não fica para trás.
- [ ] Instalar um mod novo → aparece em `Mods/00_Triagem_Novos/<mod>/`.
- [ ] Reinstalar o mesmo arquivo → é reportado como já instalado, não recopiado.
- [ ] Instalar versão diferente do mesmo mod → atualiza no lugar e guarda cópia
      em `.s4suite_backups/`.
- [ ] Um mod com pasta `Options/` pergunta qual variante instalar, e só a
      escolhida é gravada.
- [ ] Um mod que exige biblioteca (Lot51, XML Injector) mostra o aviso.
- [ ] Cancelar no diálogo não grava nada em `Mods`.
- [ ] Um `.ts4script` fica no máximo um nível abaixo de `Mods`.

### Organizador

- [ ] A árvore lista a pasta real, sem mostrar `.s4suite_staging` nem
      `.s4suite_backups`.
- [ ] Busca por nome filtra e mantém as pastas que levam até o resultado.
- [ ] Nova pasta, renomear, mover, copiar e excluir funcionam sobre a seleção.
- [ ] Mover um `.ts4script` para dois níveis abaixo avisa sobre a profundidade.
- [ ] Excluir pede confirmação e lista o que vai embora antes.
- [ ] Desativar um mod → vira `.disabled` e some da contagem do jogo.
- [ ] Painel de desativados lista, restaura e exclui em lote.
- [ ] Um `.disabled` criado à mão fora do app aparece no painel.
- [ ] Buscar duplicatas não acusa os próprios backups.
- [ ] Limpeza de lixo lista os arquivos antes de apagar.

### Merger

- [ ] Escolher origem e destino, adicionar à fila, repetir com outro grupo.
- [ ] Executar a fila processa as tarefas em ordem, com progresso avançando.
- [ ] O `.package` unificado abre no jogo e o conteúdo aparece.
- [ ] Pós-ação "manter" não toca nos originais.
- [ ] Pós-ação "desativar" renomeia os originais para `.disabled`.
- [ ] Pós-ação "backup" gera o zip **antes** de remover os originais.
- [ ] Uma tarefa que falha não interrompe as seguintes.
- [ ] O limite de tamanho por parte é respeitado.

### Tray

- [ ] Selecionar zips de Sim/Lote → a fila mostra tipo e contagem reais.
- [ ] Trocar o destino do CC de um item, e depois de todos.
- [ ] Marcar um item como pulado → ele não é importado.
- [ ] Importar → `.trayitem` e companhia vão para `Tray/`, o CC para a pasta
      escolhida.
- [ ] O jogo mostra o Sim ou Lote importado na galeria.

### Traduções

- [ ] Instalar tradução em `.package` avulso.
- [ ] Instalar tradução em `.zip` → é extraída, não copiada como zip.
- [ ] Reinstalar a mesma → reportada como já instalada.
- [ ] Excluir uma tradução da lista.
- [ ] O jogo carrega a tradução instalada.

### Painel

- [ ] Clicar no card de tamanho mostra o peso por pasta, maior primeiro.
- [ ] Clicar em scripts e em tray mostra as listas.
- [ ] Limpar cache do jogo e backup de saves funcionam.
- [ ] "Atualizar" recarrega os números.

### Configurações e i18n

- [ ] Trocar o idioma muda a interface **na hora**, sem reabrir.
- [ ] Reabrir o app preserva idioma, tema e limite do merger.
- [ ] Trocar o tema muda as cores na hora.
- [ ] Detectar automaticamente encontra a pasta do jogo.

### Robustez

- [ ] Nomes com acento, espaço e emoji não quebram nenhuma operação.
- [ ] Uma pasta `Mods` com milhares de arquivos não trava a janela.
- [ ] Fechar o app durante uma operação longa não deixa `.s4suite_staging` para
      trás na próxima abertura.

## Pendências conhecidas

- **ReShade** — adiado por decisão do usuário; precisa de mais trabalho de
  implementação. `detect_installation` e o download em `spawn_blocking` estão
  prontos e testados, o resto não foi validado. Inclui decidir o que fazer com
  o `legacy/reshade.exe` versionado no repositório.
- **Empacotamento** — o AppImage ainda é o do PyInstaller.
