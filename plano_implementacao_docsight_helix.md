# Plano de Implementação — DocSight e HelIX

## Objetivo deste documento

Este plano organiza a evolução de **DocSight** e **HELIX** como produtos de software sérios, partindo do estado atual dos repositórios e evitando expansão prematura de escopo.

A lógica é simples:

1. estabilizar o núcleo;
2. fechar contratos;
3. aumentar confiabilidade;
4. melhorar experiência de uso;
5. criar distribuição e observabilidade;
6. validar com uso real;
7. só então ampliar funcionalidades.

O objetivo não é “terminar tudo”. O objetivo é chegar a uma versão em que terceiros possam instalar, usar e confiar no comportamento do produto.

---

# Parte I — Regras gerais para os dois projetos

## Regra 1 — Não adicionar funcionalidade sem caso de uso claro

Toda nova funcionalidade deve responder:

- quem usa;
- qual problema resolve;
- qual fluxo de uso habilita;
- como será testada;
- como será diagnosticada em caso de falha.

Se uma funcionalidade não tiver resposta objetiva para essas perguntas, ela deve ficar no backlog.

---

## Regra 2 — Feature sem teste não está concluída

Uma funcionalidade só é considerada pronta quando existir pelo menos:

- teste unitário para regras locais;
- teste de integração para o fluxo real;
- teste de erro/falha;
- teste de regressão se ela corrigir um bug.

---

## Regra 3 — Feature sem diagnóstico não está concluída

O usuário precisa conseguir entender:

- o que falhou;
- onde falhou;
- por que falhou;
- se a falha é recuperável;
- o que ele pode fazer a respeito.

Evitar erros genéricos como:

```text
something went wrong
invalid document
operation failed
```

Preferir erros estruturados e acionáveis.

---

## Regra 4 — Não esconder limitações

Qualquer aproximação, perda de fidelidade, comportamento parcial ou recurso não suportado deve ser explicitado.

Exemplos:

```text
render_fidelity = "approximate"
table_detection_confidence = 0.72
unsupported_feature = "floating_text_box"
```

---

## Regra 5 — Não criar fallback silencioso

Se o caminho principal falhar, o sistema deve:

1. falhar claramente;
2. produzir diagnóstico;
3. preservar contexto suficiente para depuração.

Não trocar silenciosamente um algoritmo, parser, fonte, formato ou backend.

---

## Regra 6 — Não aceitar comportamento não determinístico onde determinismo é requisito

Para DocSight:

- mesmo arquivo;
- mesma versão;
- mesmas opções;
- mesmas fontes;
- mesmos backends;

devem produzir o mesmo resultado observável.

Para HELIX:

- mesmo estado inicial;
- mesmos inputs;
- mesma seed;
- mesma configuração;

devem produzir a mesma simulação e os mesmos hashes.

---

## Regra 7 — Não expandir arquitetura sem responsabilidade concreta

Não criar:

- crates vazios;
- abstrações especulativas;
- interfaces “para o futuro”;
- microserviços antes de necessidade real;
- camadas duplicadas.

Toda separação deve existir porque já há uma responsabilidade concreta e testável.

---

## Regra 8 — O CLI é contrato público

Mudanças em:

- nomes de comandos;
- argumentos;
- formatos JSON;
- exit codes;
- nomes de campos;
- ordenação;
- IDs;
- formatos de bundle;

devem ser tratadas como mudanças de contrato.

---

## Regra 9 — Toda fase deve ter critério de saída

Não avançar de fase porque “parece bom”.

Avançar apenas quando os critérios definidos estiverem atendidos.

---

## Regra 10 — Bugs reais viram testes permanentes

Todo bug relevante encontrado em:

- produção;
- uso manual;
- fuzzing;
- integração;
- arquivo real;
- simulação real;

deve virar fixture ou cenário de regressão.

---

# Parte II — DOCSIGHT

# Visão de produto

DocSight deve ser:

> Uma ferramenta local, headless e determinística para inspecionar, estruturar, renderizar, comparar e extrair evidência de documentos DOCX e PDF.

O produto não deve tentar ser:

- editor de documentos;
- clone do Word;
- sistema de OCR completo;
- sistema de RAG;
- motor de perguntas e respostas;
- suíte Office completa.

Esses itens podem existir futuramente, mas não são necessários para um produto inicial forte.

---

# Arquitetura-alvo simplificada

Fluxo principal:

```text
Input
  ↓
Format Detection
  ↓
Parser
  ↓
Document IR
  ↓
Layout / Geometry
  ↓
Operations
  ├── inspect
  ├── text
  ├── outline
  ├── tables
  ├── search
  ├── render
  ├── evidence
  └── diff
  ↓
CLI / Agent JSON
```

A `Document IR` deve ser o centro.

Nenhum comando deveria depender diretamente da estrutura específica de DOCX ou PDF depois da ingestão.

---

# Fase DS-0 — Congelamento de escopo

> **Status: CONCLUÍDA — 2026-09-15.**
> Entregue: `PRODUCT_SCOPE.md` (Supported / Partial / Out of scope, cada limitação ligada ao código de diagnóstico que a expõe), `BACKLOG.md` separado em v1 / post-v1 / experimental.
> Critério de saída verificado por `crates/docsight-cli/tests/product_scope.rs` (7 testes): todo comando declarado por `capabilities` aparece no escopo, nenhum comando contradiz as fronteiras, todo exit code do `ERROR_CATALOG` está documentado, e todo código de diagnóstico emitido pelo engine está no documento — e vice-versa.

## Objetivo

Definir exatamente o que pertence à primeira versão séria.

## Implementar

Criar um documento:

```text
PRODUCT_SCOPE.md
```

com três seções:

### Supported

Exemplo:

- DOCX básico;
- PDF básico;
- texto;
- headings;
- tabelas estruturais DOCX;
- tabelas inferidas PDF;
- imagens;
- hyperlinks;
- páginas;
- geometria;
- render;
- crop;
- diff;
- evidence;
- JSON.

### Partial

Exemplo:

- floating objects;
- tracked changes;
- comments;
- complex numbering;
- nested tables;
- PDF annotations;
- fonts complexas.

### Out of scope

Exemplo:

- OCR;
- editing;
- Excel;
- PowerPoint;
- macros;
- RAG;
- network service;
- cloud rendering.

## Resultado esperado

Qualquer pessoa deve conseguir responder:

> “O que exatamente DocSight promete hoje?”

## Critério de saída

- escopo documentado;
- nenhum comando existente contradiz esse escopo;
- backlog separado entre v1, post-v1 e experimental.

---

# Fase DS-1 — Estabilizar a Document IR

> **Status: CONCLUÍDA — 2026-09-15.**
> Entregue: `IrVersion { schema_version, engine_version }` em todo `Document`; JSON Schema publicado em `schemas/ir/v1/document-ir.json`; módulo `docsight-core/src/canonical.rs` com a ordenação canônica como contrato validado em ingestão (fail-closed, `BACKEND_FAILURE`); crate `docsight-ingest` como fronteira única de ingestão, eliminando seis cópias do despacho formato→parser; catálogo de fidelidade unificado em `docsight-core` (o `inspect` tinha duas listas de códigos que já divergiam).
> Bug corrigido: `parse_docx` alocava `reading_order` de figuras antes do parágrafo mas empilhava depois, produzindo IR fora de ordem que o layout mascarava ao renumerar por posição.
> Critério de saída verificado por `crates/docsight-cli/tests/ir_contract.rs` e `crates/docsight-core/tests/ir.rs`: serialização determinística, ordenação canônica (1 caso positivo + 6 negativos), IDs estáveis sob mudança de caminho, snapshots de IR e compatibilidade de schema.

## Objetivo

Transformar a IR em contrato interno estável.

## Implementar

Revisar entidades principais:

```text
Document
Page
Section
Block
Paragraph
Heading
ListItem
Table
Row
Cell
Figure
Shape
Hyperlink
Annotation
Diagnostic
Evidence
SourceLocation
Rect
ObjectId
```

Cada objeto deve ter, quando aplicável:

- ID determinístico;
- origem;
- página;
- ordem de leitura;
- bbox;
- tipo;
- conteúdo;
- confiança;
- proveniência;
- diagnóstico associado.

## Implementar versionamento

Exemplo:

```json
{
  "schema_version": "1.0",
  "engine_version": "0.4.0"
}
```

## Implementar testes

- serialização determinística;
- desserialização;
- ordenação canônica;
- IDs determinísticos;
- snapshots de IR;
- compatibilidade de schema.

## Resultado esperado

DOCX e PDF passam a produzir uma mesma representação lógica.

## Critério de saída

- nenhum comando de alto nível acessa diretamente estruturas específicas do parser;
- snapshots de IR estáveis;
- alterações incompatíveis exigem mudança explícita de versão.

---

# Fase DS-2 — Parsing robusto

> **Status: CONCLUÍDA com uma ressalva — 2026-09-15.**
> Entregue: metadados reais nos dois formatos — DOCX lê `docProps/core.xml` e `docProps/app.xml`; PDF lê o `/Info` do trailer decodificando UTF-16BE e PDFDocEncoding (antes o PDF *fabricava* `producer: "docsight-pdf-native"`, reportando todo documento como produzido pelo próprio engine). Anotações PDF passaram a ser lidas: `/Link` com `/A /URI` ou `/Dest` vira hyperlink, demais subtipos viram overlay `Annotation` (antes `/Annots` era ignorado e `links` devolvia vazio sem diagnóstico). Catálogo de 17 limites publicado em `capabilities.ingestion_limits`, com teste que falha se divergir das constantes aplicadas.
> Também corrigido: `to_document()` — o caminho canônico da IR — nunca emitia `PDF_TEXT_CODE_UNMAPPED`, embora `page()`, `trace_page()` e `rasterize()` emitissem.
> Bug de conformidade encontrado na validação do corpus real: `parse_box` rejeitava retângulos dados por cantos diagonalmente opostos, o que o ISO 32000-1 §7.9.5 declara válido e exige normalizar. Um livro de 18 MB era rejeitado com exit 11; agora extrai 1553 parágrafos, 229 headings e 256 tabelas.
> Segunda rodada de validação no corpus real, depois de classificar corretamente os arquivos sem texto: os vetos restantes eram todos do mesmo padrão — recursos que só afetam pintura abortando a extração inteira. Corrigidos: `sh` (shading), `MP`/`DP` (pontos de conteúdo marcado, sem semântica de pintura), código sem glifo em encoding simples (agora caractere de substituição com `PDF_TEXT_CODE_UNMAPPED`) e `AIS`. Form XObjects passaram a ser percorridos com a matriz e os recursos próprios, com detecção de ciclo e limite de 12 níveis. Um PDF de 11 páginas que era perda total agora entrega 191 parágrafos.
> Novo diagnóstico `PDF_PAGE_HAS_NO_TEXT_LAYER`: uma página que só pinta imagens agora diz isso explicitamente, em vez de devolver zero blocos sem explicação. Para um consumidor agente, a diferença entre “isto é um scan, precisa de OCR” e “o engine falhou” é a diferença entre parar e tentar de novo em laço.
> Placar final do corpus (2026-09-16, após o módulo de criptografia): **34 de 43 com texto extraído, 8 scans corretamente declarados como precisando de OCR, 1 rejeitado por senha de usuário desconhecida — 0 falhas não explicadas**. 430 execuções de comando: 417 sucessos, 13 erros tipados, 0 crash, 0 timeout, 0 JSON inválido.
> Módulo de criptografia PDF (`crates/docsight-pdf/src/crypt.rs`), 2026-09-16: handler de segurança padrão completo — RC4 de 40 a 128 bits (revisões 2 a 4), AES-128 (`AESV2`) e AES-256 (`AESV3`, revisões 5 e 6), chave por objeto, crypt filters (`StmF`/`StrF`/`Identity`) e decifração de strings e streams. Documentos com senha de usuário vazia — o caso comum de criptografia só de permissões — abrem direto. No corpus real, `Entre+na+plataforma.pdf` saiu de rejeitado para totalmente legível. `01-20251663927108.pdf` tem senha de usuário real e continua rejeitado corretamente (verifiquei com uma implementação de referência independente que não é bug meu). Nada é adivinhado: sem a senha certa, exit 12.
> **Ressalva:** o critério "limites configuráveis" está parcialmente atendido — os limites são centralizados, publicados e testados nas fronteiras, mas não há override por invocação. Registrado em `BACKLOG.md` com o motivo.

## Objetivo

Tornar ingestão confiável.

## DOCX

Implementar e endurecer:

- sniffing por magic bytes;
- OPC/ZIP;
- relationships;
- content types;
- document.xml;
- styles;
- numbering;
- headers;
- footers;
- comments;
- footnotes;
- endnotes;
- hyperlinks;
- images;
- tables;
- tracked changes.

## PDF

Implementar e endurecer:

- header;
- xref;
- object streams;
- page tree;
- resources;
- fonts;
- content streams;
- text operators;
- image objects;
- annotations;
- links;
- metadata.

## Segurança

Adicionar limites para:

- tamanho de arquivo;
- tamanho descomprimido;
- quantidade de ZIP entries;
- taxa de compressão;
- profundidade XML;
- número de páginas;
- imagens;
- recursos;
- tokens;
- memória;
- tempo.

## Resultado esperado

O parser não deve travar ou consumir recursos indefinidamente com arquivos hostis.

## Critério de saída

- corpus adversarial;
- fuzzing contínuo;
- zero panic em corpus conhecido;
- erros tipados para arquivos inválidos;
- limites configuráveis e testados.

---

# Fase DS-3 — Fidelidade estrutural

> **Status: CONCLUÍDA — 2026-09-15.**
> Entregue: `resource_coverage` (por página e global; denominador são figuras e recursos declarados, perdendo cobertura os que carregam placeholder, imagem não resolvida ou parte inerte) e `unsupported_feature_count` no `CoverageReport`. `overall_fidelity` mantido sobre as quatro dimensões originais — redefinir a fórmula de um número publicado sem versão nova contraria o `AGENTS.md`.
> Classificação de fidelidade corrigida: `PDF_NON_UNIFORM_STROKE_VISUAL` e `PDF_TEXT_CODE_UNMAPPED` não estavam em catálogo nenhum, então `inspect`/`coverage` reportavam fidelidade exata para documentos cujos próprios diagnósticos diziam o contrário.
> Critério de saída verificado por `crates/docsight-cli/tests/structural_fidelity.rs` (9 testes) sobre o corpus real: listas lógicas de DOCX e PDF, ordem de leitura estritamente crescente, geometria página-local dentro da caixa, confiança por bloco e detector declarado em tabelas inferidas.

## Objetivo

Garantir que a estrutura lógica esteja correta antes de perseguir fidelidade visual.

## Implementar

### DOCX

Validar:

- parágrafos;
- headings;
- listas;
- numeração;
- tabelas;
- células mescladas;
- hyperlinks;
- imagens;
- headers;
- footers;
- comentários;
- revisões.

### PDF

Validar:

- ordem de texto;
- agrupamento em spans;
- linhas;
- blocos;
- páginas;
- imagens;
- links;
- coordenadas.

## Métricas

Criar métricas como:

```text
text_coverage
structural_coverage
geometry_coverage
resource_coverage
unsupported_feature_count
```

## Resultado esperado

O usuário sabe quanto do documento foi interpretado.

## Critério de saída

- cobertura calculável;
- diagnósticos para informação perdida;
- corpus real validado manualmente.

---

# Fase DS-4 — Layout DOCX

> **Status: CONCLUÍDA PARA V1 — 2026-09-16.**
> Entregue: formatação de parágrafo real, que antes era inteiramente ignorada — alinhamento (`w:jc`), espaçamento antes/depois e entrelinha automática (`w:spacing`), e recuo esquerdo/direito/primeira-linha/pendente (`w:ind`), tudo resolvido pela cascata de estilos com propriedades diretas tendo precedência. A IR ganhou `ParagraphFormat` e subiu para `schema_version` 1.1 — mudança aditiva, versionada explicitamente.
> Consequência medida: `sample_headings.docx` passou de 3 para 4 páginas porque o documento declara `before="200" after="283"` e `line="276"`, valores que o engine substituía por um espaçamento fixo de 4pt. Os goldens foram atualizados deliberadamente depois de confirmar a origem no XML do fixture.
> `w:contextualSpacing` é lido e reportado (`DOCX_CONTEXTUAL_SPACING_IGNORED`) mas não aplicado.
> **Pendente:** objetos flutuantes, multi-seção e colunas — itens 14, 15 e 16 da ordem de implementação. Continuam no `BACKLOG.md`.

## Objetivo

Criar layout progressivamente confiável.

## Ordem de implementação

1. page size;
2. margins;
3. single section;
4. paragraph spacing;
5. text runs;
6. font metrics;
7. line breaking;
8. paragraph alignment;
9. page breaks;
10. headings;
11. lists;
12. tables;
13. headers/footers;
14. floating objects;
15. multi-section;
16. columns.

## Fontes

Implementar:

- fingerprint de fontes;
- resolução determinística;
- fallback explícito;
- diagnóstico de substituição.

## Resultado esperado

O mesmo DOCX gera a mesma geometria em ambientes equivalentes.

## Critério de saída

- golden tests de geometria;
- diferenças mensuráveis;
- substituição de fonte nunca silenciosa;
- paginação estável.

---

# Fase DS-5 — Renderização

> **Histórico de ordem — 2026-09-16.** Por decisão do usuário, DS-6 (busca, navegação e evidência) foi executada antes do fechamento desta fase. Motivo: o produto é primariamente consumido por agentes, que usam `resolve`, `context`, `query` e `focus` muito mais do que renderizam páginas.

> **Status: CONCLUÍDA PARA V1 — 2026-09-16.**
> Validação no corpus real (`Downloads`, 38 PDF + 5 DOCX): 430 execuções de comando (`inspect`, `outline`, `text`, `tables`, `overview`, `coverage`, `links`, `images`, `fingerprint`, `render`) — 404 sucessos, 26 erros tipados com envelope estruturado coerente com o exit code, **0 crashes, 0 timeouts, 0 JSON inválido**.
> Entregue: decodificador PNG nativo em `docsight-core` (greyscale, RGB, paleta com `tRNS`, greyscale+alpha e RGBA a 8 bits, filtros 0–4, não entrelaçado), e rasterização real de figuras DOCX — antes toda imagem virava caixa cinza. O `DOCX_FIGURE_RASTER_PLACEHOLDER` agora só é emitido quando o formato não é decodificável, e nomeia o formato detectado (jpeg, gif, emf, …); um PNG que falhe na decodificação em tempo de render também gera o diagnóstico, então nada fica escondido.
> Consequência medida: em `sample_features.docx` a `resource_coverage` subiu de 0.50 para 1.00 e o `unsupported_feature_count` caiu de 3 para 2.
> Testes: cobertura de área da figura na página, render repetido byte a byte idêntico, tolerância declarada explicitamente (`TOLERANCE = 0` por canal) e caminho de falha reportado.
> Continuação em 2026-09-16: JPEG agora é decodificado por um adapter Rust estreito, com `zune-jpeg` fixado em 0.5.15, modo estrito, rotinas escalares e aceleração específica de plataforma desabilitada. Os limites de 40 milhões de pixels e 100 scans progressivos são publicados em `capabilities.ingestion_limits`. JPEG malformado continua explícito por `DOCX_FIGURE_RASTER_PLACEHOLDER`, sem segundo decoder ou degradação silenciosa.
> Os critérios de saída da v1 estão cobertos pelos goldens geométricos e visuais, render e crop pela mesma transformação, repetição byte a byte, corpus real sem crash e visual diff determinístico. Demais formatos, variantes PNG de 1/2/4/16 bits e entrelaçadas, e filtro de escala (a ampliação é nearest-neighbour, determinística mas sem suavização) ficam explicitamente em post-v1 no `BACKLOG.md`.

## Objetivo

Transformar a IR em visual confiável.

## Implementar

Pipeline:

```text
Document IR
↓
Display List
↓
Page Transform
↓
Rasterizer
↓
PNG
```

## Suportar

- texto;
- imagens;
- linhas;
- boxes;
- tabelas;
- fills;
- borders;
- crop;
- DPI variável.

## Testes

- pixel golden;
- tolerância definida;
- render repetido;
- crop por bbox;
- crop por ObjectId.

## Resultado esperado

Renderização torna-se evidência visual reproduzível.

## Critério de saída

- render estável;
- nenhum crash no corpus;
- diferenças visuais detectáveis;
- PNG determinístico onde possível.

---

# Fase DS-6 — Busca, navegação e evidência

> **Histórico de ordem — 2026-09-16: esta fase foi executada antes do fechamento da DS-5.** Ordem praticada: DS-0 → DS-1 → DS-2 → DS-3 → DS-4 → **DS-6** → DS-5 → DS-7. DS-4, DS-5 e DS-6 estão concluídas para o escopo v1; codecs e filtros adicionais permanecem post-v1.
> **Status: CONCLUÍDA — 2026-09-16.**
> Entregue: `overview`, `focus`, `peek`, `context`, `resolve`, DQL espacial, busca literal e regex com filtros por tipo/página/região e resultados na menor granularidade endereçável, inclusive células. Todo ID emitido resolve em `evidence` e `context`; objetos geométricos seguem para `crop`, e objetos sem geometria falham com erro tipado que nomeia o bloco âncora. Projeção por budget, NDJSON e continuação determinística cobrem payloads grandes.
> Critério de saída verificado pelos testes `navigation_contract`, M12, M16, M17 e M18: fluxos de localizar→contextualizar→recortar→provar, IDs fechados, saída repetível e cenários de economia de interação em uma ou duas chamadas, sem parser descartável.

## Objetivo

Transformar a ferramenta em algo útil para automação e agentes.

## Implementar

### Navegação

- overview;
- outline;
- focus;
- peek;
- context;
- resolve;
- hit test.

### Busca

- texto literal;
- regex;
- página;
- tipo de objeto;
- região;
- bbox.

### Evidence

Cada resultado importante deve poder retornar:

```text
object id
page
bbox
text
source
confidence
fidelity
diagnostics
optional crop
```

## Resultado esperado

Um agente ou script consegue encontrar informação e verificar visualmente sua origem.

## Critério de saída

- fluxos end-to-end testados;
- IDs estáveis;
- navegação consistente;
- evidência reproduzível.

---

# Fase DS-7 — Diff sério

> **Status: CONCLUÍDA — validada em 2026-09-16.**
> Entregue: comparação de pacote, semântica e visual; registros de lineage separados de `ObjectId`; mudanças adicionadas, removidas, modificadas e movidas; ambiguidade e evidência limitada expostas; relatórios humano, JSON e NDJSON com continuação vinculada aos dois documentos e ao perfil visual.
> Critério de saída verificado pelos 12 testes de `m8_diff_cli` e pelos testes unitários de `docsight-diff`: documentos idênticos produzem zero mudanças, uma inserção não causa cascata de falsos positivos, imagens alteradas e mudanças visuais são detectadas, e execuções repetidas produzem saída determinística.

## Objetivo

Tornar comparação de documentos uma funcionalidade de produto.

## Implementar três níveis

### Package diff

Comparar estrutura interna de DOCX/PDF.

### Semantic diff

Comparar:

- textos;
- blocos;
- headings;
- tabelas;
- imagens;
- metadados.

### Visual diff

Comparar renderizações.

## Saída

Exemplo:

```json
{
  "changed_pages": [2, 3],
  "added_objects": 4,
  "removed_objects": 2,
  "modified_objects": 11
}
```

## Resultado esperado

DocSight passa a ser útil em QA documental e pipelines CI.

## Critério de saída

- diffs determinísticos;
- falso positivo controlado;
- testes com documentos reais;
- relatório legível por humanos e máquinas.

---

# Fase DS-8 — CLI e UX

> **Status: CONCLUÍDA — 2026-09-16.**
> Entregue: quickstart agent-first de cinco minutos, catálogo documentado de toda a superfície canônica, completions determinísticas para Bash, Elvish, Fish, PowerShell e Zsh e descoberta do utilitário por `capabilities`. Como completion é shell source, `--agent`, `--ndjson` e limites de saída são rejeitados com erro tipado e stdout vazio; não há mistura de protocolos nem fallback. PDFs com senha conhecida usam `--password-file`: o segredo não entra em `argv`, saída, trace ou proof bundle, é limitado a 127 bytes e propagado por inspeção, busca, render, diff, bundles, replay e verificação.
> O comando canônico de busca permanece `find`. O nome `search` do plano original foi corrigido, sem criar alias legado. A sincronização entre `--help`, `capabilities`, `PRODUCT_SCOPE.md` e `README.md` é verificada por teste.
> Critério de saída validado por `cli_ux`, `product_scope`, `agent_protocol_cli`, conformidade M15, Clippy sem warnings e a suíte integral do workspace com todas as features.

## Objetivo

Fazer o produto parecer ferramenta profissional.

## Implementar

Padronizar comandos:

```text
docsight inspect
docsight text
docsight outline
docsight tables
docsight render
docsight diff
docsight evidence
docsight find
docsight verify
docsight completions
```

## Padronizar

- help;
- mensagens;
- exit codes;
- stderr;
- stdout;
- JSON;
- NDJSON;
- quiet mode;
- limites;
- continuation token.

## Resultado esperado

Usuário novo consegue usar sem ler código.

## Critério de saída

- quickstart de 5 minutos;
- CLI consistente;
- shell completion;
- documentação de todos comandos;
- erros claros.

---

# Fase DS-9 — Performance e escala

> **Status: CONCLUÍDA — 2026-09-16.**
> Entregue: harness versionado `xtask benchmark` em release, processos isolados, medição de parse, layout DOCX, normalização PDF, render, pico de memória, tamanho de saída e throughput. Os cenários cobrem DOCX pequeno e médio, 5.000 blocos, PDF de 1, 100 e 1.000 páginas e PDF com stream de imagem RGB 2048×2048.
> Orçamentos Linux x86_64/Rust 1.96.0 são revisados em arquivo, nunca regravados pelo benchmark. A CI falha por regressão de tempo, memória, saída, throughput, contagem ou crescimento de RSS e publica o relatório medido como artefato. Limites de segurança continuam sendo descobertos por `capabilities`; o benchmark não os amplia.
> Critério de saída validado por `cargo run --release -p xtask -- benchmark --check`, testes dos geradores de escala, Clippy sem warnings e a suíte integral do workspace.

## Objetivo

Garantir comportamento previsível.

## Medir

- arquivo pequeno;
- médio;
- grande;
- 1 página;
- 100 páginas;
- 1000 páginas;
- DOCX com milhares de blocos;
- PDF com imagens grandes.

## Métricas

```text
parse_time
layout_time
render_time
peak_memory
output_size
objects_per_second
pages_per_second
```

## Resultado esperado

Usuário sabe limites práticos.

## Critério de saída

- benchmarks versionados;
- regressões detectadas na CI;
- nenhum crescimento de memória não controlado.

---

# Fase DS-10 — Distribuição

## Objetivo

Eliminar fricção de instalação.

## Produzir

- Windows x64;
- Linux x64;
- Linux ARM64;
- macOS Intel;
- macOS Apple Silicon.

## Automatizar

- build;
- test;
- checksum;
- changelog;
- release artifacts.

## Resultado esperado

Instalação em minutos.

## Critério de saída

Um usuário sem Rust instalado consegue executar DocSight.

---

# Fase DS-11 — Beta real

## Objetivo

Colocar o produto na mão de terceiros.

## Selecionar

5–10 usuários técnicos.

## Coletar

- arquivos que falham;
- comandos confusos;
- erros;
- performance;
- features realmente usadas.

## Regra

Nenhum bug de beta é corrigido sem teste de regressão.

## Resultado esperado

Roadmap passa a ser guiado por uso real.

---

# Fase DS-12 — V1 estável

## Critérios

DocSight v1 só existe quando:

- instalação é simples;
- comportamento é documentado;
- corpus amplo passa;
- crashes são raríssimos;
- contratos estão versionados;
- JSON estável;
- render confiável;
- diff confiável;
- diagnósticos são claros;
- segurança foi testada;
- releases automatizadas.

---

# Parte III — HELIX

# Visão de produto

HELIX deve ser:

> Uma plataforma para executar, registrar, reproduzir, ramificar e investigar simulações distribuídas determinísticas.

O diferencial principal não deve ser apenas “simulação distribuída”.

O diferencial deve ser:

> conseguir reproduzir e explicar exatamente onde e por que duas execuções divergiram.

---

# Fluxo central do produto

```text
run
↓
record
↓
snapshot
↓
replay
↓
fork
↓
compare
↓
bisect
↓
inspect divergence
```

Esse deve ser o caminho principal.

---

# Fase HX-0 — Reposicionamento do produto

## Objetivo

Definir o que HELIX é e o que não é.

## Criar

```text
PRODUCT_SCOPE.md
```

## In scope

- deterministic simulation;
- event recording;
- snapshots;
- replay;
- forks;
- branch comparison;
- divergence inspection;
- distributed execution;
- fault injection;
- diagnostic bundles.

## Fora inicialmente

- generic game engine;
- MMO engine;
- generic orchestration system;
- Kubernetes replacement;
- general-purpose distributed database;
- generic workflow engine.

## Resultado esperado

Arquitetura deixa de crescer sem direção.

---

# Fase HX-1 — Deterministic Core

## Objetivo

Fazer determinismo ser incontestável.

## Validar

- fixed-point math;
- deterministic RNG;
- stable ordering;
- tick model;
- input ordering;
- event ordering;
- state hashing;
- snapshots.

## Testes

Rodar a mesma simulação:

- 10 vezes;
- 100 vezes;
- em máquinas diferentes;
- Windows/Linux;
- diferentes níveis de paralelismo.

Comparar:

```text
final state hash
checkpoint hashes
snapshot bytes
event order
```

## Resultado esperado

Mesmos inputs sempre produzem mesmo estado.

## Critério de saída

Zero divergência não explicada.

---

# Fase HX-2 — Event Store

## Objetivo

Tornar execução reproduzível após crash.

## Implementar/fortalecer

- append-only log;
- segmentação;
- checksums;
- snapshots;
- index;
- fsync policy;
- recovery;
- corruption detection.

## Cenários

- kill abrupto;
- partial write;
- snapshot incompleto;
- índice corrompido;
- segmento corrompido.

## Resultado esperado

HELIX consegue recuperar a última execução válida.

## Critério de saída

Crash recovery automatizado e testado.

---

# Fase HX-3 — Run Artifact

## Objetivo

Criar uma unidade portátil de execução.

Definir um formato de bundle, por exemplo:

```text
run.hlxb
```

## Deve conter

- scenario;
- seed;
- engine version;
- protocol version;
- event log;
- snapshots;
- hashes;
- topology;
- fault schedule;
- plugin metadata.

## Resultado esperado

Uma execução pode ser entregue para outra pessoa e reproduzida offline.

## Critério de saída

Bundle reproduz execução em outra máquina.

---

# Fase HX-4 — Replay UX

## Objetivo

Fazer replay ser simples.

## CLI

Exemplo:

```text
helix replay run.hlxb
helix replay run.hlxb --tick 81200
helix replay run.hlxb --from 80000 --to 82000
```

## Permitir

- seek;
- step;
- pause;
- inspect;
- export state.

## Resultado esperado

Debug temporal útil sem interface gráfica.

---

# Fase HX-5 — Forks e branches

## Objetivo

Permitir testar alternativas históricas.

## CLI

```text
helix fork run.hlxb --tick 81200 --name alternative
```

## Permitir

- trocar input;
- trocar plugin;
- trocar fault schedule;
- trocar config permitida.

## Resultado esperado

Usuário consegue perguntar:

> “O que teria acontecido se esse input não tivesse ocorrido?”

---

# Fase HX-6 — Divergence Inspector

## Objetivo

Fazer essa ser a feature principal do HELIX.

## Hierarquia

```text
Global
↓
Shard
↓
Entity
↓
Component
↓
Field
```

## CLI

```text
helix diff run-a.hlxb run-b.hlxb
helix bisect run-a.hlxb run-b.hlxb
```

## Resultado esperado

Saída semelhante:

```text
First divergence:
tick: 81342

shard: world-02
entity: player-184
component: Velocity
field: x

expected: 1.2500
actual:   1.2539
```

## Adicionar causalidade

Listar inputs anteriores relevantes.

## Critério de saída

HELIX consegue apontar a primeira divergência exata em cenário conhecido.

---

# Fase HX-7 — Distributed Runtime

## Objetivo

Tornar cluster robusto sem sacrificar determinismo.

## Validar

- QUIC;
- reconnect;
- backpressure;
- ordering;
- mTLS;
- peer identity;
- transport failure.

## Cenários

- peer cai;
- peer volta;
- atraso;
- reorder;
- perda;
- conexão lenta.

## Resultado esperado

Falhas de transporte não criam estado silenciosamente inconsistente.

---

# Fase HX-8 — Sharding e ownership

## Objetivo

Garantir autoridade consistente.

## Validar

- shard owner;
- leases;
- epochs;
- migrations;
- handoff;
- duplicate prevention;
- zombie prevention.

## Invariantes

```text
one shard → one active owner
one entity → one authoritative location
epoch always increases
```

## Resultado esperado

Nenhuma entidade duplicada após migração.

---

# Fase HX-9 — Fault Lab

## Objetivo

Transformar chaos testing em recurso de produto.

## Faults

- latency;
- packet loss;
- partition;
- freeze;
- crash;
- corruption;
- delayed input;
- node restart.

## Requisito

Mesmo fault schedule + mesma seed = mesmo resultado.

## CLI

```text
helix run scenario.toml --faults faults.toml
```

## Resultado esperado

Falhas podem ser reproduzidas exatamente.

---

# Fase HX-10 — WASM Plugins

## Objetivo

Permitir extensão segura.

## Isolar

- memória;
- CPU/fuel;
- capabilities;
- filesystem;
- network;
- RNG.

## Capabilities

Exemplo:

```text
world.read
world.command
rng.stream
```

## Resultado esperado

Plugin hostil não derruba o node.

---

# Fase HX-11 — Time Debugger Desktop

## Objetivo

Criar visualização depois da CLI estar sólida.

## Interface

Painéis:

- timeline;
- tick;
- entities;
- shards;
- events;
- inputs;
- divergence;
- branches;
- faults.

## Resultado esperado

Usuário consegue visualizar uma divergência sem interpretar logs crus.

## Regra

A GUI não deve conter lógica que não exista no core.

---

# Fase HX-12 — Observabilidade

## Objetivo

Fazer comportamento do cluster ser explicável.

## Expor

- tick rate;
- queue sizes;
- backpressure;
- node status;
- shard owners;
- migrations;
- event rate;
- snapshot time;
- replay speed.

## Resultado esperado

Operador consegue entender gargalos.

---

# Fase HX-13 — Escala

## Objetivo

Descobrir limites reais.

## Cenários

```text
1 node / 1k entities
1 node / 10k
1 node / 50k
3 nodes / 10k
5 nodes / 20k
10 nodes / 100k
```

Não assumir que todos serão metas obrigatórias.

Primeiro medir.

## Métricas

```text
tick duration
events/sec
network bytes/sec
snapshot time
replay speed
memory
CPU
migration duration
```

---

# Fase HX-14 — Beta técnico

## Objetivo

Colocar HELIX nas mãos de pessoas que trabalham com:

- distributed systems;
- simulation;
- multiplayer backend;
- deterministic engines;
- testing.

## Perguntas

- entenderam o produto?
- reproduziram um bug?
- conseguiram criar cenário?
- conseguiram interpretar divergence?
- quais comandos faltaram?

---

# Fase HX-15 — V1

HELIX v1 só deveria existir quando:

- determinismo está demonstrado;
- replay é confiável;
- bundles são portáveis;
- divergence funciona;
- cluster é estável;
- fault injection é reproduzível;
- CLI é documentada;
- instalação é simples;
- CI multiplataforma passa.

---

# Parte IV — Prioridade recomendada

## Prioridade geral

Sugestão:

```text
DocSight: 75%
HELIX:    25%
```

até DocSight atingir beta.

Depois:

```text
DocSight: 50%
HELIX:    50%
```

---

# Parte V — Ordem prática de implementação

## DocSight — próximas 10 entregas

1. fechar PRODUCT_SCOPE;
2. estabilizar Document IR;
3. schemas/versionamento;
4. corpus real e adversarial;
5. parser hardening;
6. métricas de cobertura;
7. layout DOCX básico confiável;
8. render pipeline;
9. diff sólido;
10. release cross-platform.

---

## HELIX — próximas 10 entregas

1. fechar PRODUCT_SCOPE;
2. reforçar testes determinísticos;
3. estabilizar formato de run;
4. estabilizar bundle `.hlxb`;
5. replay CLI;
6. fork;
7. diff;
8. temporal bisect;
9. divergence inspector;
10. beta técnico.

---

# Parte VI — Regras de branch e entrega

## Branches

Evitar branches longas.

Preferir:

```text
main
feature/<nome>
fix/<nome>
refactor/<nome>
```

## Pull requests

Cada PR deve responder:

```text
O que mudou?
Por que mudou?
Qual contrato foi afetado?
Como foi testado?
Quais riscos existem?
```

---

# Parte VII — Definition of Done

Uma tarefa só termina quando:

- código implementado;
- testes adicionados;
- testes existentes passam;
- lint passa;
- formatação passa;
- erro tratado;
- comportamento documentado;
- contrato atualizado;
- regressão adicionada se aplicável;
- benchmark executado se afetar performance.

---

# Parte VIII — Regras de releases

Usar Semantic Versioning.

```text
0.x.y
```

durante desenvolvimento.

Exemplo:

```text
0.1.0 núcleo inicial
0.2.0 novos recursos compatíveis
0.2.1 correção
```

Antes de 1.0, mudanças incompatíveis ainda podem ocorrer, mas nunca silenciosamente.

---

# Parte IX — Regra de congelamento

Quando uma fase começar:

> não adicionar funcionalidade fora daquela fase, salvo bug crítico.

Isso é especialmente importante para HELIX.

---

# Parte X — Métrica real de progresso

Não medir progresso por:

- linhas de código;
- crates;
- comandos;
- features listadas.

Medir por:

```text
quantos fluxos reais funcionam ponta a ponta;
quantos casos de erro são tratados;
quantos documentos/simulações reais passam;
quantos bugs antigos nunca voltaram;
quantas pessoas externas conseguem usar sem ajuda.
```

---

# Resumo final

## DocSight

Caminho:

```text
Scope
→ IR
→ Parsing
→ Structure
→ Layout
→ Render
→ Evidence
→ Diff
→ UX
→ Performance
→ Distribution
→ Beta
→ V1
```

## HELIX

Caminho:

```text
Scope
→ Deterministic Core
→ Event Store
→ Run Artifact
→ Replay
→ Fork
→ Diff
→ Divergence
→ Cluster
→ Fault Lab
→ Plugins
→ Debugger
→ Scale
→ Beta
→ V1
```

O princípio central para os dois:

> parar de provar que o projeto consegue fazer muitas coisas e começar a provar que as coisas que ele já faz são confiáveis o suficiente para outras pessoas dependerem delas.
