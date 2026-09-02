# AGENTS.md

## Escopo e fonte de verdade

- Leia `Projeto_DOCSIGHT_Especificacao.docx` antes de tomar decisões arquiteturais ou alterar contratos públicos.
- Trate a especificação como fonte de verdade do produto. Em caso de conflito, siga nesta ordem: solicitação explícita atual do usuário, este `AGENTS.md`, especificação do projeto e convenções já presentes no código.
- Preserve o objetivo central: o DOCSIGHT é uma camada de evidência local e headless para inspecionar DOCX e PDF por terminal, sem depender de Word, Excel, LibreOffice, automação COM ou serviços remotos.
- Não amplie o escopo por iniciativa própria. DOCX editing, XLSX, PowerPoint, reflow de gráficos Office, perguntas em linguagem natural e modo daemon/rede estão fora da v1.

## Regras obrigatórias do usuário

- Não adicione comentários ao código sem solicitação explícita do usuário. Isso inclui comentários de linha, de bloco, comentários explicativos, TODOs, FIXMEs e doc comments. Prefira nomes precisos, funções pequenas, tipos claros e documentação externa quando ela for solicitada.
- Nunca execute `git commit`, `git push`, crie tags, publique releases, abra pull requests ou reescreva o histórico sem autorização explícita do usuário para a ação específica.
- Não crie nem mantenha fallbacks funcionais. Não adicione aliases legados, rotas alternativas, compatibilidade retroativa silenciosa, valores substitutos, degradação automática ou caminhos secundários.
- Se o caminho canônico falhar, retorne um erro explícito, preserve o diagnóstico e opere em modo fail-closed.
- Remova fallbacks existentes quando estiverem dentro do escopo da tarefa. Rollback transacional para restaurar o estado anterior após falha continua permitido.
- Não esconda recursos não suportados, aproximações, perda de evidência ou redução de fidelidade. Exponha diagnósticos tipados com consequência e objetos afetados.
- Não altere arquivos alheios à tarefa. Preserve mudanças existentes do usuário e nunca descarte trabalho sem autorização.

## Princípios do produto

- Ofereça três visões sincronizadas do documento: estrutural, textual e visual.
- Normalize DOCX e PDF em uma única `Document IR` imutável. Depois da ingestão, comandos e serviços não devem acessar estruturas específicas do formato diretamente.
- Trate texto, layout, geometria, relações, recursos e evidência renderizada como dados de primeira classe.
- Use pontos PDF, 1/72 de polegada, coordenadas locais da página e origem no canto superior esquerdo como sistema geométrico público.
- Gere IDs determinísticos a partir do digest do documento, identidade da origem e caminho semântico normalizado. Nunca use UUID aleatório, endereço de memória, ordem de thread ou estado global instável.
- Para os mesmos bytes, mesma versão do engine, mesmos backends, fontes e opções, produza JSON e artefatos determinísticos.
- Mantenha execução local e offline como regra. Parsing, inspeção e renderização não podem exigir rede.
- Não busque hyperlinks externos nem execute macros, OLE ou conteúdo incorporado.
- Diferencie fatos estruturais de inferências. Todo resultado inferido deve carregar confiança e proveniência.

## Arquitetura esperada

Use um workspace Rust com responsabilidades separadas, sem dependências circulares:

- `docsight-cli`: interface `clap`, validação de argumentos e apresentação humana.
- `docsight-agent`: schemas JSON/NDJSON, limites de saída e tokens de continuação.
- `docsight-core`: IDs, geometria, `Document IR`, proveniência e diagnósticos.
- `docsight-ooxml`: leitura OPC/ZIP e parsing OOXML limitado por recursos.
- `docsight-layout`: paginação e layout determinístico de DOCX.
- `docsight-pdf`: abstração do backend PDF e fronteira FFI com MuPDF fixado.
- `docsight-render`: display lists, rasterização, crop e contact sheets.
- `docsight-fonts`: descoberta, shaping e resolução determinística de fontes.
- `docsight-tables`: modelo canônico, inferência de tabelas PDF e exportadores.
- `docsight-search`: texto, regex e futuramente DQL.
- `docsight-diff`: comparação de pacote, semântica e visual.
- `docsight-cache`: cache endereçado por conteúdo e fingerprint de reprodução.
- `docsight-worker`: isolamento de parsers e backends nativos.
- `native`: integração reproduzível do MuPDF fixado.
- `fixtures`: documentos mínimos, adversariais e goldens.
- `schemas`: snapshots versionados dos contratos públicos.
- `xtask`: automação de build, release e atualização explícita de goldens.

Não crie todos os crates vazios antecipadamente. Introduza cada crate quando houver responsabilidade concreta e teste correspondente. Tipos de bibliotecas externas não podem vazar para a IR ou para os schemas públicos; encapsule-os em adapters.

## Ordem de implementação

- Siga os marcos M0 a M9 e os primeiros 15 issues descritos na especificação.
- Comece por um recorte mínimo e verificável: sniffing por magic bytes, erros tipados, estruturas centrais, OPC limitado, semântica DOCX básica, PDF básico e só então layout progressivamente mais amplo.
- Para o primeiro layout DOCX, suporte deliberadamente uma seção, uma fonte, parágrafos e tamanho de página explícito. Exija snapshot geométrico determinístico e PNG antes de ampliar a superfície OOXML.
- Entregue incrementos verticais completos, com contrato, implementação, diagnóstico, teste e documentação solicitada. Evite scaffolding especulativo.
- Não antecipe DQL, OCR, busca vetorial ou outros plugins antes de existir uma interface estável e uma necessidade do marco atual.

## Práticas de código Rust

- Use Rust estável e mantenha a versão mínima suportada declarada no workspace quando o projeto for inicializado.
- Formate com `cargo fmt` e trate `cargo clippy --all-targets --all-features` sem warnings antes de concluir uma mudança.
- Prefira tipos de domínio, enums exaustivos, newtypes e invariantes validadas a strings soltas, mapas genéricos e booleanos ambíguos.
- Mantenha funções pequenas, coesas e com uma única responsabilidade. Separe parsing, validação, normalização, layout, apresentação e I/O.
- Evite duplicação, estado global mutável, efeitos implícitos e abstrações prematuras.
- Propague erros com contexto tipado. Não use `unwrap`, `expect`, `panic!`, `todo!` ou `unimplemented!` em caminhos de produção.
- Não ignore `Result`, warnings, elementos desconhecidos nem conversões potencialmente truncadas.
- Use conversões verificadas para tamanhos, offsets, índices e aritmética de limites. Trate overflow como erro explícito.
- Restrinja `unsafe` ao menor módulo possível, preferencialmente a wrappers FFI auditáveis. Exponha uma API Rust segura e teste entradas inválidas e falhas do backend.
- Faça concorrência somente em unidades independentes. Ordene resultados e diagnósticos antes de expô-los para manter saída byte a byte estável.
- Mantenha APIs públicas mínimas. Mudanças em schema, ID, coordenadas, exit codes ou ordenação exigem testes de contrato e decisão explícita.

## Dependências e build

- Adicione dependências apenas quando houver necessidade concreta e verifique manutenção, licença, superfície de ataque e suporte multiplataforma.
- Fixe backends nativos e componentes cujo comportamento afete renderização ou determinismo. Versione `Cargo.lock` para o binário.
- Não introduza Word, LibreOffice, Excel, COM, `unoconv`, conversores remotos ou chamadas de rede como dependência de execução.
- Mantenha MuPDF atrás de uma fronteira pequena e substituível. A geometria e a rasterização PDF autoritativas devem vir de uma única versão fixada do backend.
- Fontes substitutas devem ser determinísticas e declaradas em diagnóstico. Nunca selecione silenciosamente uma fonte arbitrária do sistema.
- O build e os testes devem funcionar em Windows, Linux e macOS, respeitando diferenças de filesystem sem alterar contratos observáveis.

## Parsing e segurança

Considere todo documento uma entrada hostil.

- Detecte formato por magic bytes; nunca confie apenas na extensão.
- Limite quantidade de entradas, tamanho comprimido e descomprimido, taxa de compressão, profundidade XML, tamanho de tokens, páginas, imagens, memória, CPU e iterações de layout.
- Desabilite entidades XML externas e qualquer resolução de recurso externo.
- Normalize caminhos OPC e rejeite caminhos absolutos, traversal com `..`, relações inválidas e colisões ambíguas.
- Valide dimensões e bytes decodificados antes de alocar imagens.
- Preserve elementos OOXML desconhecidos como nós opacos ligados ao objeto mais próximo e emita diagnóstico. Não finja que o documento foi totalmente interpretado.
- Isole o backend PDF e decoders nativos em worker quando o modo de segurança exigir. Converta falha do worker em erro tipado.
- Nunca execute macros, objetos OLE, JavaScript PDF, anexos ou conteúdo ativo.

## Contrato da Document IR

- A IR é imutável após a fase correspondente ser congelada e preserva proveniência de cada objeto.
- Blocos visíveis devem carregar, quando conhecível: ID, tipo, página, `bbox`, z-index, ordem de leitura, origem e confiança.
- Modele explicitamente documento, metadados, estilos, seções, páginas, blocos, overlays e recursos.
- Preserve parágrafos, headings, itens de lista, tabelas, figuras, shapes, hyperlinks, bookmarks, comentários, notas, alterações controladas e campos suportados.
- Não descarte informação para simplificar exportação. Exporte uma forma reduzida apenas quando o usuário solicitar e a perda estiver explícita.
- Tabelas DOCX são estruturais; preserve grid, spans, conteúdo aninhado, estilo, coordenadas e geometria. Tabelas PDF são inferidas e sempre expõem confiança e detector.

## Layout e renderização

- Implemente o layout como fases determinísticas: geometria de seção, fontes, shaping, linhas, parágrafos, listas, tabelas, objetos flutuantes, paginação, headers/footers e congelamento final.
- Respeite page breaks, `keep-with-next`, `keep-lines`, viúvas/órfãs, colunas, spans e quebras de linha dentro da cobertura declarada.
- Associe cada aproximação a um código diagnóstico, consequência provável, confiança reduzida e objetos afetados.
- Use a mesma transformação de página para geometria, render, crop, hit-testing e diff.
- Crop por objeto deve derivar coordenadas do `bbox` sem exigir que o consumidor estime pixels.
- Não declare fidelidade a Word onde ela não existe. Meça e exponha a fidelidade alcançada.

## CLI e protocolo de agente

- Trate `stdout` como API. Em modo agente, emita apenas JSON ou NDJSON válido, sem decoração, progresso ou ANSI.
- Envie diagnósticos e progresso somente para `stderr`. `--quiet` deve removê-los quando aplicável.
- Mantenha schemas explicitamente versionados e faça validação por JSON Schema.
- Garanta ordenação canônica por página, ordem de leitura e ID.
- Operações grandes devem oferecer NDJSON e limites rígidos como `--max-bytes`, `--max-items` e `--text-limit`, com truncamento explícito e token determinístico de continuação.
- Não altere silenciosamente nomes de campos, semântica, unidades, IDs, exit codes ou formato de erros.
- Use os exit codes definidos na especificação: `0`, `2`, `10`, `11`, `12`, `13`, `20`, `21`, `30` e `40` para suas categorias correspondentes.
- Erros devem permitir decisão programática sem comparação de texto e incluir contexto suficiente para ação.
- Modo humano é uma projeção dos mesmos registros tipados; não implemente lógica de produto separada na camada de apresentação.

## Cache e reprodutibilidade

- Enderece cache pelo digest exato do arquivo, versão do engine, versões dos backends, fingerprint das fontes, perfil de layout e opções relevantes.
- Nunca reutilize resultado com fingerprint incompatível.
- Escritas de cache devem ser atômicas. Corrupção ou incompatibilidade deve gerar erro explícito ou invalidação explícita do item, nunca uso silencioso de dado possivelmente incorreto.
- Não inclua timestamps, caminhos absolutos locais ou ordem de execução em saídas determinísticas, salvo quando o contrato exigir e o campo estiver claramente separado.

## Testes e critérios de conclusão

Toda mudança deve ser testada no nível adequado.

- Testes unitários para parsing, IDs, geometria, normalização, limites e erros.
- Testes de propriedade para OPC, caminhos, relacionamentos, cascata de estilos e estruturas sujeitas a combinações adversariais.
- Fixtures mínimas para cada comportamento e fixtures malformadas para cada defesa.
- Snapshots determinísticos para IR, schemas, CLI e geometria.
- Goldens visuais com tolerâncias explícitas para renderização; não atualize goldens apenas para fazer testes passarem sem investigar a diferença.
- Testes de integração para separação stdout/stderr, exit codes, limites, continuação e execução offline.
- Execuções paralelas repetidas devem produzir JSON byte a byte idêntico.
- Fuzzing para container OPC, relacionamentos OOXML, estilos, numbering, grid de tabelas, layout de parágrafo, agrupamento de spans PDF e parser DQL quando existir.
- Teste limites imediatamente acima e abaixo do valor permitido, não apenas o caso feliz.
- Uma mudança só está concluída quando formatação, lint, testes relevantes e contratos afetados passam. Se algo não puder ser executado, informe exatamente o comando, o motivo e o que permaneceu sem validação.

## Fluxo de trabalho do agente

1. Leia a solicitação, este arquivo, a parte relevante da especificação e os arquivos envolvidos antes de editar.
2. Verifique o estado atual do diretório e preserve alterações do usuário.
3. Delimite o menor incremento que resolve completamente a solicitação.
4. Identifique contratos, riscos de segurança, determinismo, limites e testes afetados.
5. Implemente usando o caminho canônico, sem fallback e sem comentários no código.
6. Execute `cargo fmt`, `cargo clippy --all-targets --all-features`, testes direcionados e, quando proporcional ao risco, `cargo test --workspace --all-features`.
7. Revise o diff final para detectar mudanças acidentais, vazamento de tipos externos, saídas não determinísticas, caminhos alternativos e arquivos gerados indevidos.
8. Relate de forma objetiva o que mudou, quais verificações passaram e qualquer limitação real. Não faça commit.

## Conduta ao modificar contratos

- Não preserve contrato antigo por alias ou compatibilidade silenciosa. Se a mudança solicitada for incompatível, altere o caminho canônico e atualize todos os consumidores dentro do escopo.
- Schemas públicos precisam de versão explícita. Uma nova versão não autoriza manter duas rotas implícitas; a seleção deve ser clara e deliberada.
- Mudanças de ID, geometria, ordenação, serialização, diagnóstico, limites e cache exigem fixtures e snapshots específicos.
- Se uma decisão contrariar a especificação, pare e peça autorização antes de implementar.

## Documentação e comunicação

- Mantenha nomes, mensagens de erro, ajuda da CLI e schemas em inglês, coerentes com a especificação, salvo solicitação contrária.
- Escreva documentação externa somente quando fizer parte da tarefa ou for necessária para um contrato público alterado.
- Não use comentários no código como substituto para uma API clara.
- Ao concluir trabalho substancial, mencione brevemente apenas melhorias realmente relevantes que ainda possam ser feitas em arquitetura, performance ou serviços Docker opcionais. Trate-as como recomendações e não as implemente sem solicitação.
