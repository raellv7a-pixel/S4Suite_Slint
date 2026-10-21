# Merge/unmerge: Sims 4 Studio e implementação Linux

## Alvo e autoridade das conclusões

Alvo inspecionado: `Sims4Studio_v3.2.6.5 (Star)/S4Studio.dll`, assembly
3.2.6.5, SHA-256
`28d77b36473da0d0e0e6a34c9a15ffe497a26db399eade2d19dd42fcd5d90efd`,
MVID `b805aeb1-2ea9-436f-ae03-76a6a6cb1be4`.

REA confirmou a identidade e o alvo .NET 6 na evidência
`ev_f635c64071bef9386450bf83fa66170bff05d7a4bc0345b26ee1ec9b4a13576e`
(resumo `ev_0a5091145f8dbbd113c64c95ab7ef629cf8791320ac1d693b85ca5d4c60771cd`).
A inspeção de membros REA ficou indisponível por limite de representação de
64 MiB (`ev_45bbf6cc4efa7c23232ad80562beeaf9a006f7df84b4cc1a0c9f2278f0c847ba`).
Por isso, os detalhes abaixo foram obtidos por leitura estática do CIL com
`dnfile`/`dncil`, sem carregar/executar a assembly. Tokens/RVAs referem-se
exclusivamente a esse build. As bibliotecas Python foram usadas apenas na
investigação; a implementação do aplicativo é Rust nativo.

A aplicação Windows/WPF não foi executada. Não foi comprovada equivalência de
toda a aplicação nem realizada validação dentro do jogo. O escopo observado é o
pipeline de packages e seus view models, não todas as proteções globais do Studio.

## Processo observado

1. `PackageMergeUtility.Merge` (`06008688`, RVA `0xACAAC`) reúne entradas por
   TGI, registra os originais em uma árvore de pastas/packages e calcula tamanho
   com dados e entradas de índice. Rejeita tamanho acima de `2_147_483_647`
   bytes com a mensagem `Merged package cannot exceed 2GB`. Esse é um limite
   observado do Studio, não uma prova de overflow ou do limite interno do jogo.
2. `ProcessMergedPackage` (RVA `0xACEF0`) copia os bytes armazenados dos recursos,
   sem descomprimir/recomprimir malhas ou texturas. Se a chave já estiver na
   saída, a entrada posterior não substitui a primeira. Isso **não** comprova
   igualdade de payloads: um conflito divergente pode perder conteúdo.
3. `S4SMergedPackageManifest` registra nomes, subpastas e TGIs. O merge também
   sabe incorporar a árvore de um package anteriormente mesclado. O predicado
   `06008694`, RVA `0xAD3F4`, exclui o tipo do manifesto anterior da estimativa
   de recursos comuns. O merge grava os dados, índice e cabeçalho; a camada UI
   chama `Defragment` ao terminar.
4. `PackageMergeViewModel` exige estado pronto e mais de um package para habilitar
   seu comando (`get_CanMerge`, RVA `0x1624B6`). O worker (`0600BDA7`, RVA
   `0x1626BC`) verifica cancelamento entre packages, registra falhas de carga,
   atualiza progresso e continua com os que carregou. A rotina de cancelamento
   (`0600BDA8`, RVA `0x1628C4`) apaga o arquivo de saída selecionado quando existe.
5. A seleção de unmerge verifica `DBPFPackage.IsMerged` e avisa quando não é um
   merged (`0600BDC1`, RVA `0x163080`). O comando exige origem/destino não vazios.
   O view model escolhe uma nova pasta com sufixo numérico quando necessário,
   usa estado ocupado e escreve `Un-merged package information.txt`
   (`0600BDC5`, RVA `0x163358`).
6. `UnmergeMergedFolder` (RVA `0xAD03C`) percorre a árvore, localiza cada TGI,
   copia dados armazenados, tamanho descomprimido e compressão para novos DBPFs.
   Uma entrada ausente é pulada, não causa erro nesse método. Arquivos são
   reconstruídos; não há informação suficiente para recuperar o layout binário
   original ou versões de recursos já descartadas por colisões.

Essas observações não estabelecem proteção contra symlinks, traversal, alterações
concorrentes ou bombas de descompressão em outros caminhos do Studio. Não se
atribuiu ao Studio as proteções extras implementadas no S4Suite.

## Formato interoperável

- Chave do manifesto: tipo `0x7FB6AD8A`, grupo e Instance zero; versão 1.
- Manifesto em zlib `0x5A42` ou sem compressão. Strings são Pascal32: comprimento
  em bytes `u32 LE`, seguido de UTF-8. Confirmado pelos métodos de string
  `06008806`/`0600882F`, encoding `0A000762 = Encoding.get_UTF8`, e helpers
  `0600880C`/`06008839`.
- Corpo: versão `u32`; pasta raiz com nome, quantidade de subpastas, subpastas
  recursivas, quantidade de packages; cada package contém nome sem extensão,
  quantidade de recursos, e recursos `(Instance u64 LE, Type u32 LE, Group u32 LE)`.
- Índice DBPF: primeira palavra de Instance é a parte alta, segunda a baixa.
  Por compatibilidade interna, os campos existentes do projeto ainda se chamam
  `instance_low` (palavra alta) e `instance_ex` (palavra baixa); o codec de manifesto
  faz a conversão explícita. Flags de constantes seguem essa mesma ordem.
- Cabeçalho escrito: DBPF 2.1, quantidade de entradas no offset 36, tamanho do
  índice em 44, minor do índice 3 em 60, posição do índice em 64.
- `ReadIndexTable` (`06008599`, RVA `0xAAC1C`) lê compressão/committed `u16`
  **somente** se bit 31 do tamanho armazenado estiver ativo. O writer novo emite
  essa extensão com bit ativo e committed 1, conforme package real inspecionado.

## Decisões no S4Suite

| Aspecto | Comportamento implementado |
|---|---|
| Fila | Preserva execução sequencial e pós-ações existentes |
| Tamanho | Limite configurado inclui dados, manifesto, cabeçalho e índice; teto de compatibilidade de 2 GiB menos 1 byte; margem de 10% para agrupamento |
| Recurso indivisível | Se um recurso mais overhead não cabe, falha antes da publicação; não gera parte acima do limite |
| Memória | Cópia em streaming dos snapshots e comparação de saída em buffers; não carrega uma parte inteira em RAM |
| Duplicatas | Deduplica somente bytes e metadados iguais; mantém todas as associações aos originais no manifesto |
| Conflito TGI | Conteúdo/metadados divergentes abortam o merge/unmerge; não copia o descarte silencioso do Studio |
| Já mesclados | Mantém bloqueio por marcadores exatos de nome e acrescenta detecção pelo manifesto no snapshot; desunificar primeiro |
| Caminhos | Preserva nomes Unicode e subpastas; rejeita componentes inseguros, traversal, caminhos absolutos e nomes duplicados ambíguos |
| Unmerge incompleto | Recurso ausente ou manifestos incompatíveis abortam o lote sem publicar originals parciais |
| Publicação | Merge mantém no-replace, relatório e rollback existentes; unmerge publica o lote inteiro em nova pasta `Unmerged_<timestamp>[_N]` por rename no-replace |
| Originais | Unmerge nunca modifica nem apaga entradas; pós-ações de merge continuam exigindo hashes e autorização existentes |
| Parser | Limites de bytes, contagens, profundidade e expansão de caminhos; recusa versão desconhecida, zlib truncado/extra e tamanhos declarados divergentes |
| UI | Ação “Desunificar packages...” na aba Merger, seleção múltipla, destino fora de Mods, progresso, erro e resultado; bloqueio de operações simultâneas |

O parser limita manifesto codificado/descomprimido a 64 MiB, strings a 64 KiB,
profundidade a 64, pastas a 65.536, packages a 262.144, associações de recursos a
2.000.000 e soma dos caminhos expandidos a 64 MiB. São políticas defensivas locais,
não limites inferidos do formato S4S.

### Partes e limitações

Cada parte nova carrega o manifesto completo do lote. No unmerge do S4Suite,
**selecione todas as partes**: os índices são reunidos antes de reconstruir cada
original. Isso permite detectar partes ausentes. O Studio, pelo método observado,
pula recursos ausentes; desunificar uma parte isolada nele não recupera o original
que atravessou várias partes. Não foi testada a importação no aplicativo Windows.

Packages antigos do S4Suite sem manifesto não podem ser desunificados pelo novo
processo. Não se inventam nomes nem grupos de recursos. Packages externos que já
perderam recursos por colisão não recuperam esses dados: o manifesto S4S não
contém cópia nem hash dos payloads históricos.

Não se reproduziu a rotina do Studio que apaga a saída no cancelamento. A UI atual
não oferece interrupção do lote em execução. A regra preservada é terminar a
transação sem expor arquivos incompletos; as fontes permanecem intactas no unmerge.

## Como usar

- **Merge:** selecione origem e destino, configure tamanho nas configurações,
  escolha a pós-ação, adicione grupos à fila e execute. Manter originais é a opção
  conservadora para conferir o conteúdo antes de qualquer remoção.
- **Unmerge:** na aba Merger, clique em **Desunificar packages...**, selecione o
  merged do Studio ou todas as partes do S4Suite e escolha destino fora de Mods.
  O resultado aparece numa subpasta nova; o diálogo informa os caminhos finais.
- Isso não instala conteúdo. A política de blacklist e rejeição preventiva de
  merged continua valendo para qualquer instalação posterior em Mods.

## Verificação executada

- `cargo test`: 264 testes aprovados em 15 suítes, incluindo regressões de
  compartilhamento de TGI, Unicode/subpastas com mesmo nome, partes ausentes,
  limite total, colisões, parser malformado e preservação dos arquivos existentes.
- Smoke independente com `[oydis] Nuit Jewelry Set MERGED.package` existente:
  extraídos os 6 packages e verificadas as 338 associações declaradas por comparação
  exata de TGI, payload armazenado, tamanho descomprimido e compressão.
- Os 6 resultados foram novamente mesclados em **2 partes** com limite de
  2.000.000 bytes e desunificados. Comparação exata dos recursos por original passou;
  o arquivo externo de entrada permaneceu byte-a-byte igual.
- Janela Slint real renderizada na aba Merger: botão e aviso visíveis, fila e
  pós-ações preservadas. O compositor manteve 944×1026 apesar dos pedidos de
  redimensionamento; não se afirma teste de duas larguras diferentes.
- Seletores nativos de arquivo/pasta não foram automatizados. Não foram instalados
  packages em Mods, removidos merged existentes ou executados o jogo e o Studio.
