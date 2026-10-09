# Validação dos motores — Rust + Slint

O port só é dado como concluído quando **todo motor** passar nas três camadas
abaixo. Testes automatizados provam que o engine faz a coisa certa e que o botão
chega até ele; nenhum deles prova que o resultado agrada num `Mods` real de
verdade, com centenas de mods e nomes acentuados.

## Camadas

1. **Engine** — suíte de integração por motor, sem event loop (`tests/*_flow.rs`).
2. **Bridge** — `tests/bridge_wiring.rs`: todo callback tem handler dos dois
   lados, e acioná-lo muda o disco.
3. **Sessão real** — `examples/simulacao_usuario.rs`: a janela de verdade,
   dirigida contra uma cópia da pasta `Mods` do usuário. Roda com
   `S4_SANDBOX=<dir> cargo run --example simulacao_usuario` e imprime um
   relatório de 90 verificações. É o que o roteiro manual abaixo automatiza.

## Estado automatizado

`cargo test --offline` — **167 testes, todos verdes** após a correção do
Smart Mod Installer V4. Smoke gráfico com dados sintéticos: três ZIPs e um
package avulso, sete payloads corretos após decisões todos/único/manual,
resumo prévio e staging limpo. Nenhum dado real foi usado nessa validação.

Registro anterior: `simulacao_usuario` contra 287 packages e 95 scripts reais —
**90 verificações, todas verdes**; essa sessão não foi repetida na correção do V4.

| Motor | Arquivo | Testes | Camada 1 | Camada 2 |
|---|---|---:|:---:|:---:|
| Instalador | `installer_flow.rs` | 25 | ✅ | ✅ |
| Organizador | `organizer_flow.rs` | 28 | ✅ | ✅ |
| Merger | `merger_flow.rs` | 17 | ✅ | ✅ |
| Tray | `tray_flow.rs` | 12 | ✅ | ✅ |
| Traduções | `translations_flow.rs` | 12 | ✅ | — |
| DBPF | `dbpf_flow.rs` | 11 | ✅ | n/a |
| Estatísticas | `stats_flow.rs` | 6 | ✅ | ✅ |
| ReShade | `reshade_flow.rs` | 8 | ⚠️ parcial | — |
| i18n | `i18n_coverage.rs` | 7 | ✅ | ✅ |
| Temas | `theme_system.rs` | 11 | ✅ | ✅ |
| Bridge | `bridge_wiring.rs` | 17 | n/a | ✅ |
| Unitários | `src/core/*` + `src/engine/*` | 13 | ✅ | n/a |

Mods desativados (`engine/disabled.rs`) são cobertos dentro de
`organizer_flow.rs`, que é onde o motor é usado.

## O que a camada 3 pegou

A sessão simulada encontrou sete problemas que nenhum teste de motor via, todos
corrigidos:

- **merger sobrescrevia o resultado de uma tarefa com o da seguinte** —
  `Merged_Content_<segundos>_PartNNN` colidia entre tarefas que terminavam no
  mesmo segundo, e a pós-ação já tinha consumido os originais. A saída agora
  leva o nome do grupo e o prefixo é reservado antes da primeira parte.
- **tema só mudava na abertura seguinte** — o handler gravava no config e não
  tocava na global do Slint.
- **staging de uma sessão interrompida sobrevivia dentro de `Mods`**, onde o
  jogo tentaria carregá-lo. Limpo na montagem da janela.
- **tradução avulsa virava `01_Traducoes/mod_x___tradu__o_pt_br/`** e a lista da
  aba exibia esse nome no lugar do arquivo. Arquivo solto agora vai direto para
  a pasta, e a identidade de instalação preserva acento e maiúscula.
- **corrigir profundidade de script movia arquivos sem perguntar**, ao contrário
  do resto do organizador. Passa pelo diálogo de confirmação com a lista.
- **executar a fila de novo repetia tarefas concluídas** — só as pendentes rodam.
- **o aviso de dependência nunca aparecia** — a busca por `Lot51` era feita nos
  bytes crus, e os recursos DBPF vêm em zlib (0 acertos em 1569 packages reais).
  Agora os recursos pequenos são descomprimidos antes da busca.

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
- [ ] Um pacote de roupas com palavras como `options` no nome do ZIP/arquivo
      ou `Optional` numa pasta não obriga a escolher uma peça.
- [ ] Uma pasta interna `Options/` oferece possíveis variantes, sem afirmar
      incompatibilidade: instalar todos, escolher apenas um ou seleção manual.
- [ ] Vários ZIPs e pastas de opções na mesma fila geram decisões independentes;
      desmarcar arquivos de um grupo não remove arquivos comuns nem outros grupos.
- [ ] Seleção manual aceita múltiplos arquivos; seleção vazia não avança.
- [ ] Depois das decisões, a fila, as contagens e as dependências do resumo
      refletem a seleção. Nada é gravado até confirmar o resumo final.
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
- **Instalador de arquivo avulso** — um `.package` solto ganha uma pasta com o
  nome dele dentro de `00_Triagem_Novos`. Diferente do caso das traduções, aqui
  a pasta agrupa o mod e não atrapalha a leitura; fica como está até alguém
  reclamar.
- **Verificação no jogo** — que o `.package` unificado e a tradução instalada
  aparecem dentro do The Sims 4 continua sendo teste de olho humano.
