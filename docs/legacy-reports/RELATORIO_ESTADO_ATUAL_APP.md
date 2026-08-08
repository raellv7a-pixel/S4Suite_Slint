# Relatorio de Estado Atual do S4 Suite Qt

Projeto analisado: `S4Suite_Qt/`  
Data da analise: 2026-06-04  
Escopo: arquitetura do app, fluxos principais, operacoes destrutivas, persistencia, empacotamento e validacao local.

## Resumo executivo

O app esta em um estado melhor que os relatorios anteriores indicavam. Varias correcoes importantes ja existem no codigo atual: salvamento atomico do `config.json`, whitelist basica para exclusoes, rollback no instalador, `unique_dest_path()` no Tray Importer, relatorio parcial do Merger, debounce na busca do organizador e inclusao de `locales` no PyInstaller.

Ainda assim, o app trabalha diretamente com arquivos reais do usuario: mods, saves, pastas Tray, DLLs do jogo e presets. Por isso, os maiores riscos restantes estao em validacao de caminho, operacoes parcialmente reversiveis, mensagens de sucesso que escondem falhas e ausencia de uma suite de testes automatizados para cenarios de erro.

## Validacao executada

- `python3 -m py_compile S4Suite_Qt/*.py S4Suite_Qt/ui/*.py`: passou sem erros.
- `QT_QPA_PLATFORM=offscreen python3 S4Suite_Qt/run_and_check.py`: falhou porque o Python global nao tem `PyQt6`.
- `QT_QPA_PLATFORM=offscreen ./S4Suite_Qt/build_env/bin/python S4Suite_Qt/run_and_check.py`: falhou porque o venv local esta quebrado e nao encontra `libpython3.14.so.1.0`.

## Estado dos processos principais

### Inicializacao e navegacao

O app principal cria uma `QMainWindow`, aplica tema salvo, monta uma sidebar e carrega todas as abas no inicio. A estrutura e simples e compreensivel. O risco e que qualquer erro no construtor de uma aba derruba a inicializacao inteira, ja que todas as abas sao instanciadas diretamente.

Nivel de preocupacao: medio.

Correcao recomendada: proteger a criacao das abas individualmente e mostrar uma tela de erro por aba quando um modulo falhar.

### Configuracoes e persistencia

`ConfigManager` usa cache e salva com arquivo temporario + `os.replace()`, o que e positivo. Tambem preserva config corrompida como `.corrupt`.

Nivel de preocupacao: baixo/medio.

Risco restante: a UI salva cada campo com chamadas separadas de `ConfigManager.set()`. Se uma gravacao falhar no meio, a configuracao pode ficar parcialmente atualizada.

Correcao recomendada: montar o dicionario completo em memoria e chamar `ConfigManager.save()` uma unica vez na tela de configuracoes.

### Instalador de mods

O instalador extrai arquivos com `7z`, bloqueia extensoes perigosas, detecta opcoes mutuamente exclusivas, tenta localizar updates e possui rollback em erro.

Nivel de preocupacao: alto.

Riscos principais:

- A extracao com `7z x` para uma pasta temporaria nao valida explicitamente se todos os caminhos extraidos ficaram dentro do diretorio temporario.
- O matching por nome limpo + similaridade ainda pode classificar mods diferentes como update se os nomes forem muito parecidos.
- O rollback protege bastante, mas ainda trabalha diretamente no destino final antes da instalacao terminar.
- Excecoes inesperadas em extracao que nao sejam `ValueError`/`RuntimeError` viram "nenhum arquivo util", perdendo a causa real do erro.

Correcoes urgentes:

- Validar todos os caminhos extraidos com `realpath/commonpath` antes de processar ou mover.
- Fazer instalacao em staging dentro de `Mods/.s4suite_staging` e aplicar troca final so depois de tudo validado.
- Trocar retorno simples por resultado estruturado com status, mensagem, arquivos instalados, arquivos removidos e rollback executado.
- Tornar update por heuristica uma sugestao, nao uma verdade. Confirmar com manifest/hash quando existir.

### Organizador

O organizador carrega a arvore em `QThread`, faz busca com debounce, permite desativar mods renomeando para `.disabled`, mover itens para categorias, limpar lixo, detectar duplicatas e corrigir scripts profundos.

Nivel de preocupacao: alto.

Riscos principais:

- Mover arquivos/pastas para uma categoria aceita qualquer caminho configurado pelo usuario, sem validar se e uma pasta permitida ou desejada.
- A correcao automatica de scripts move para `00_Scripts_Corrigidos` sem usar `unique_dest_path()`, podendo falhar ou sobrescrever dependendo do ambiente/FS.
- A deteccao de duplicatas considera apenas tamanho + nome, nao hash. O texto da UI chama de duplicata exata, mas tecnicamente nao e.
- A limpeza de lixo remove extensoes amplas como `.ini`; em mods, alguns `.ini` podem ser configuracoes relevantes.
- Falhas internas sao impressas no console, mas nem sempre chegam ao usuario.

Correcoes urgentes:

- Validar destino das categorias antes de mover: deve existir, ser diretorio, nao ser home/raiz, e preferencialmente estar dentro de `Mods` ou ser explicitamente confirmado.
- Usar `unique_dest_path()` tambem na correcao automatica de scripts.
- Confirmar duplicata com hash completo antes de sugerir exclusao.
- Fazer a limpeza de lixo em modo "preview" com lista revisavel antes de apagar.
- Mostrar resumo de falhas de movimentacao/limpeza na UI.

### Tray Importer

O Tray Importer separa arquivos Tray de `.package`, usa cache SQLite em WAL, evita duplicata confirmando SHA256 completo apos fast hash, e usa `unique_dest_path()` nos moves.

Nivel de preocupacao: medio.

Riscos principais:

- A importacao nao e transacional. Se falhar apos mover arquivos Tray mas antes de mover todos os packages, o estado fica parcialmente importado.
- `import_name` pode ficar vazio apos sanitizacao, gerando pasta `Imported_Sims/`.
- O banco usa `INSERT OR REPLACE` com `filepath UNIQUE`, mas sem guardar informacoes de lote/importacao para rollback ou auditoria.

Correcoes recomendadas:

- Adicionar transacao logica por importacao: manifest do lote, arquivos movidos e status final.
- Definir fallback seguro quando `import_name` sanitizado ficar vazio.
- Salvar relatorio de importacao por lote.

### Merger

O Merger ja retorna resultado estruturado, salva `merge_report.json`, faz `fsync`, fecha handles corretamente e trata falhas parciais.

Nivel de preocupacao: medio.

Riscos principais:

- O parsing DBPF assume leituras completas; arquivos truncados podem gerar excecoes tratadas como skip, mas sem validacao detalhada.
- Em erro fatal, arquivos `Merged_Content_Part*.package` parcialmente gravados podem permanecer no destino.
- O destino manual pode ser qualquer pasta escolhida pelo usuario.

Correcoes recomendadas:

- Gravar output em arquivo temporario e renomear para `.package` apenas apos `write_chunk()` concluir.
- Remover ou marcar arquivos parciais em erro fatal.
- Validar destino e avisar quando estiver fora de `Mods`/triagem.

### ReShade

O modulo usa manifesto com hash para a DLL instalada, cria backup de DLL existente e preserva DLLs que nao parecem ReShade. Isso e uma boa evolucao.

Nivel de preocupacao: alto.

Riscos principais:

- Instala ReShade baixando de URL fixa, sem verificacao de hash/assinatura.
- A versao esta hardcoded em `5.9.2`; pode ficar obsoleta ou indisponivel.
- Presets podem copiar `.ini`, `.fx` e `.png` sobrescrevendo arquivos existentes sem backup/renomeio.
- Desinstalacao remove `reshade-shaders` inteiro, mesmo que parte tenha sido adicionada manualmente pelo usuario.

Correcoes urgentes:

- Verificar hash conhecido do instalador ou permitir instalador local escolhido pelo usuario.
- Guardar manifesto tambem para presets e shaders instalados.
- Fazer backup/`unique_dest_path()` antes de copiar preset.
- Na remocao, remover apenas arquivos registrados pelo manifesto ou pedir confirmacao detalhada.

### Dashboard

O Dashboard calcula estatisticas, limpa cache e cria backup de saves em background.

Nivel de preocupacao: medio.

Pontos positivos:

- Backup usa `get_documents_dir()`, com `xdg-user-dir` e fallback.
- Limpeza de cache acumula falhas e mostra mensagem se algo nao foi removido.

Riscos restantes:

- Estatisticas percorrem toda a pasta Mods e podem ser caras em bibliotecas grandes.
- Backup usa `shutil.make_archive()` direto no destino final; se falhar, pode deixar zip parcial.

Correcoes recomendadas:

- Criar backup em arquivo temporario e renomear ao final.
- Adicionar cancelamento/progresso para estatisticas em bibliotecas grandes.

### Build e distribuicao

O `build.sh` cria venv, instala `pyinstaller`/`PyQt6`, inclui `locales` e monta AppImage.

Nivel de preocupacao: alto para reprodutibilidade.

Riscos principais:

- `pip install pyinstaller PyQt6` sem versoes fixas torna o build nao reprodutivel.
- `wget` baixa `appimagetool` sem verificacao de integridade.
- O `build_env` atual do workspace esta quebrado.
- O script remove bibliotecas internas do AppImage; isso pode corrigir conflitos em uma maquina e quebrar em outra.

Correcoes urgentes:

- Fixar versoes em `requirements.txt`/`requirements-build.txt`.
- Verificar SHA256 do `appimagetool`.
- Recriar o venv quebrado e documentar a versao Python suportada.
- Testar o AppImage em ambiente limpo.

## Correcoes urgentes priorizadas

### P0 - Antes de usar em dados importantes

1. Validar todos os caminhos extraidos por `7z` antes de mover/processar.
2. Implementar staging + commit final para instalador e backup de saves.
3. Fazer preview/relatorio para limpeza de lixo e exclusao de duplicatas.
4. Corrigir duplicatas do organizador para usar hash completo antes de chamar algo de "exato".
5. Criar manifesto para ReShade presets/shaders e parar de remover `reshade-shaders` inteiro sem auditoria.

### P1 - Proxima rodada

1. Salvar configuracoes em uma unica chamada atomica.
2. Validar caminhos configurados de categorias e destino manual do merger.
3. Gravar outputs do Merger em `.tmp` e renomear ao final.
4. Corrigir `build_env` e fixar dependencias.
5. Adicionar relatorios estruturados de instalacao/importacao/movimentacao.

### P2 - Qualidade e manutencao

1. Adicionar testes unitarios para `s4common`, `s4installer`, `s4tray`, `s4merger`.
2. Adicionar testes de integracao com arquivos temporarios simulando Mods/Tray/Game/Bin.
3. Remover imports nao usados e arquivos legados obsoletos.
4. Padronizar logs e mensagens de erro traduziveis.
5. Reduzir estilos inline e concentrar tema em `s4theme.py`.

## Conclusao

O app ja tem uma base funcional e varias protecoes importantes. O estado atual nao parece "quebrado", mas ainda nao esta no nivel ideal para uma ferramenta que apaga, move, compacta e sobrescreve arquivos de usuario. A prioridade tecnica deve ser seguranca operacional: staging, manifestos, validacao de caminhos, preview antes de deletar e builds reprodutiveis.
