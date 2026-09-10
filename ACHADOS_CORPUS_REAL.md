# Achados em corpus real (Downloads)

Sessão de teste exploratório do DOCSIGHT contra documentos reais, não sintéticos. O corpus Windows original e o corpus Linux atual são conjuntos diferentes; seus números não devem ser comparados arquivo a arquivo.

- Engine: `0.1.4`, build `--release`, toolchain 1.96.0.
- Corpus histórico: `C:\Users\crist\Downloads` — 38 PDF + 5 DOCX = 43 arquivos.
- Reauditoria e revalidação atual: `/home/gustavo/Downloads` — 62 PDF + 33 DOCX = 95 arquivos, em 2026-09-10.
- Método histórico: fluxo de agente completo (`capabilities` → `inspect` → `text`/`outline`/`overview`/`tables`/`table`/`page`/`query`/`resolve`/`context`/`hit`/`render`/`crop`/`diff`/`evidence`/`bundle`/`verify`), com e sem `--sandbox`, em modo humano e `--agent`.
- Fase 1 quick-wins (A2, A3, A4, A5, A7): CONCLUÍDA após reauditoria. A2 agora implementa `/Font` e falha em chave realmente desconhecida; A4 preserva os oito modos no raster e no trace; A5 pinta o espelhamento em vez de apenas normalizar o bbox.
- Fase 2 tabelas+resolve (A9, A10, A11): CONCLUÍDA após reauditoria. A11 ganhou o estado canônico `low_confidence`; candidato fraco não é mais promovido a `resolved` nem confundido com empate.
- Fase 3 (A6, A12, A14, A15, A16, A17): CONCLUÍDA após reauditoria. A6 registra o objeto efetivamente pintado e não acusa recurso sem uso; A15 entrega `page`, `object`, `operator` e `offset` tipados no envelope do agente.
- A revalidação também encontrou e corrigiu três lacunas PDF independentes: `/Contents` indireto apontando para array, padding de hex ímpar em CMap e `h` repetido depois de `re`. Elas elevaram o `inspect` de 79 para 90 arquivos; o tratamento explícito de CMap vazio elevou o total a 91, mantendo a capacidade textual indisponível em vez de inventar um mapeamento.

## Placar histórico antes das correções

| Resultado | Arquivos | Observação |
| --- | --- | --- |
| Usável | 10 | 23% do corpus |
| Rejeitado com exit != 0 | 9 | 2 encriptados (correto), 7 malformed |
| Parseia mas `capabilities.text = false` | 16 | um recurso não suportado zera o documento |
| Parseia mas extrai zero texto | 8 | conteúdo dentro de Form XObject |

## Placar atual em `/home/gustavo/Downloads`

| Operação | Resultado | Observação |
| --- | --- | --- |
| `inspect` | 91/95 com exit 0 | quatro recusas duras e tipadas |
| `text` | 60/95 com exit 0 | 30 PDF e 30 DOCX completam a Document IR textual |
| `text` não vazio | 54/95 | seis PDFs restantes são A8/Form XObject |
| `render` da página 1 a 36 dpi | 29/91 inspecionáveis | 23 PDF + 6 DOCX; 62 recusas `LAYOUT_PARTIAL`, principalmente A1/A1b, fontes e cobertura do renderer |

As quatro recusas no `inspect` são: um PDF criptografado (exit 12), um arquivo com extensão PDF sem magic bytes (exit 10) e dois DOCX com altura de figura igual a zero (exit 11). O PDF com mapeamento ToUnicode vazio agora inspeciona com a capacidade textual explicitamente indisponível e `text` falha com exit 20. Nenhum desses casos recebe dado substituto ou perda silenciosa.

## Diagnóstico central histórico — resolvido nos itens concluídos

Onze arquivos são perdidos pelo mesmo erro de granularidade: o engine trata **"não sei pintar isso"** como **"não sei nada sobre este documento"**. `SA`, soft mask, `Tr 3`, `Tz` negativo e um `Tj` vazio não afetam evidência textual nem geometria de objeto, e mesmo assim vetam o documento inteiro.

O projeto já tem a máquina certa para esses casos: `coverage`, `fidelity`, `capability_details`, diagnóstico tipado com consequência e objetos afetados. Rebaixar `visual.fidelity` para `unsupported` com o código do recurso, mantendo `text` e `structure` exatos, é mais honesto que o veto atual — hoje `inspect` de um currículo devolve `paragraphs: null`, o que informa **menos** sobre o documento do que "o texto é exato, a pintura tem um recurso não suportado".

Isso não afrouxa a regra de fail-closed nem introduz fallback: o caminho canônico continua único, só que a consequência passa a ser proporcional ao que de fato não é suportado. A reauditoria confirmou e corrigiu os pontos em que o primeiro fechamento ainda era apenas nominal: curinga de `ExtGState`, semântica visual incompleta de `Tr`/espelhamento, diagnósticos sem objeto, candidato fraco promovido e erro PDF sem localização tipada.

## Prioridade sugerida

| # | Achado | Arquivos | Esforço estimado | Status |
| --- | --- | --- | --- | --- |
| A1 | `xref streams` não suportado | 4 | alto | pendente |
| A2 | Chave desconhecida de ExtGState mata o documento | 6 | baixo | CONCLUÍDO (Fase 1) |
| A3 | `Rect::new` rejeita largura zero (`() Tj`) | 4 | baixo | CONCLUÍDO (Fase 1) |
| A9 | Texto de célula de tabela sai destruído | todas as tabelas PDF | médio | CONCLUÍDO (Fase 2) |
| A4 | Qualquer `Tr != 0` é fatal | 2 | baixo | CONCLUÍDO (Fase 1) |
| A5 | `Tz` negativo é fatal | 1 | baixo | CONCLUÍDO (Fase 1) |
| A7 | DOCX: target OPC absoluto tratado como traversal | 2 | baixo | CONCLUÍDO (Fase 1) |
| A6 | Soft masks fatais | 3 | médio | CONCLUÍDO (Fase 3, com cascata) |
| A8 | Form XObjects não percorridos | 8 | alto | pendente |
| A12 | PNG praticamente sem compressão | todos os renders | baixo | CONCLUÍDO (Fase 3) |
| A13 | Sem cache entre chamadas | todo fluxo de agente | alto | pendente |
| A10, A11, A14–A17 | Calibração, contrato e diagnóstico | — | baixo | CONCLUÍDOS (Fases 2–3) |

---

## A1 — `xref streams` não suportado

**Impacto:** 4 arquivos. Formato padrão de cross-reference desde o PDF 1.5 (2003); é a maioria dos PDFs modernos, não um caso exótico.

**Sintoma:** `inspect` retorna sucesso mas com `capabilities.text/structure/render = false`, `paragraphs: null`, `tables: null`, e warning `LAYOUT_PARTIAL: feature is not supported: xref streams`. Qualquer outro comando falha com exit `20`.

**Causa raiz:** `crates/docsight-pdf/src/syntax.rs:70`.

**Arquivos:** `01-20251663927108_.pdf`, `5abcd890-9baa-4ee1-bae9-3802f1e848a4.pdf`, `9781801073394-MOBILE_APP_REVERSE_ENGINEERING.pdf`, `declaração-não-vinculo (1).pdf`.

**Repro:**
```
docsight --agent inspect "9781801073394-MOBILE_APP_REVERSE_ENGINEERING.pdf"
```

**Direção:** implementar a leitura de xref stream (tipo `/XRef`, campos `W`, `Index`, subseções, objetos em `/ObjStm`). É o item de maior custo da lista e o de maior retorno.

**Relacionado:** `hybrid xref streams` (`syntax.rs:125`) e `incremental PDF updates` (`syntax.rs:120`, ver A1b).

### A1b — `incremental PDF updates`

**Impacto:** 1 arquivo (`1000037462.pdf`). Comum em PDF assinado ou anotado, onde o writer apenda uma nova seção em vez de reescrever.

**Causa raiz:** `crates/docsight-pdf/src/syntax.rs:120`.

---

## A2 — Chave desconhecida de ExtGState mata o documento — CONCLUÍDO (Fase 1)

**Status:** corrigido, reauditado e validado. Todas as chaves padronizadas de `ExtGState` cobertas pelo parser têm tratamento explícito. `/Font [font size]` seleciona a fonte e o tamanho ativos; `SA`/`SM`/`HT`/`TR`/`BG`/`UCR` geram `PDF_EXTGSTATE_IGNORED` somente quando o estado é aplicado a conteúdo pintado. Uma chave realmente desconhecida falha com exit 20, sem curinga permissivo. `SMask` e `BM` não-Normal seguem o tratamento proporcional de A6; `AIS`/`OP` true continuam fail-closed.

**Impacto:** 6 arquivos, todos por causa de `SA` — *stroke adjustment*, um booleano de hinting de rasterização que não afeta geometria nem texto.

**Sintoma:** `LAYOUT_PARTIAL: feature is not supported: PDF ExtGState entry SA`, com `text=false` e contagens nulas. Um flag cosmético apaga o documento inteiro.

**Causa raiz:** `crates/docsight-pdf/src/content.rs:2129` — o braço `_ =>` do match de chaves de ExtGState retorna `UnsupportedFeature` para qualquer chave não enumerada. Qualquer chave rara ou futura tem o mesmo efeito.

**Arquivos:** `CV_GUSTAVO_MARTINS.pdf`, `CV_GUSTAVO_MARTINS_.pdf`, `CV_GUSTAVO_MARTINS_KJC.pdf`, `CV_GUSTAVO_MARTINS_UNITY.pdf`, `DiplomaDigital.pdf`, `Assets_being_removed_March_31st.pdf`.

**Repro:**
```
docsight --agent inspect CV_GUSTAVO_MARTINS.pdf
```

**Direção:** classificar as chaves de ExtGState por consequência, não por conhecimento:

- chaves que só afetam rasterização (`SA`, `FL`, `RI`, `TR`/`TR2`, `HT`, `SM`, `BG`, `UCR`, `D`, `LW`, `LC`, `LJ`, `ML`) → aceitar, e quando alterarem o pixel, registrar diagnóstico e rebaixar apenas `visual`;
- chaves que afetam evidência (`Font`, `SMask` com máscara real, `BM` não-Normal, `CA`/`ca`) → tratamento próprio;
- chave desconhecida → `UnsupportedFeature` tipado com o nome da chave e exit 20; não presumir silenciosamente que seu efeito é apenas visual.

---

## A3 — `Rect::new` rejeita largura zero e um `() Tj` derruba o documento — CONCLUÍDO (Fase 1)

**Status:** corrigido e validado. Run de avanço zero virou no-op em `content.rs` antes do `Rect::new`; erros restantes agora carregam localização tipada completa conforme A15.

**Impacto:** 4 arquivos rejeitados com exit `11` (`MALFORMED_DOCUMENT`).

**Sintoma:** `malformed document: text operator produced invalid geometry`, sem página, offset ou objeto na mensagem.

**Causa raiz:** `crates/docsight-pdf/src/content.rs:882` constrói o bbox do text run e mapeia qualquer erro para `malformed`. `crates/docsight-core/src/lib.rs:270` recusa `x0 >= x1 || y0 >= y1`. Um text-show que não avança (string vazia, glifo de avanço zero) produz `min_x == max_x` e derruba o documento.

**Evidência medida (contagem de operadores de text-show vazios por arquivo):**

| Arquivo | `() Tj` / `[] TJ` vazios |
| --- | --- |
| `Imposto_Casa.pdf` | 184 |
| `arquivo (1).pdf` | 55 |
| `boletim.pdf` | 1 |
| `f116e911-d4d3-4628-9915-2fd36ace6cd5.pdf` | 1 |

`boletim.pdf` é o caso limite: **um único operador que não pinta nada** invalida o arquivo inteiro.

**Repro:**
```
docsight inspect boletim.pdf   # exit 11
```

**Direção:** um text run de avanço zero não é geometria inválida, é um no-op. Ignorar o run (sem emitir objeto) ou emitir com bbox degenerado explicitamente marcado, em vez de abortar o documento. Se `Rect` precisa manter a invariante de área positiva, o filtro deve ficar em `content.rs` antes da construção.

---

## A4 — Qualquer `Tr != 0` é fatal — CONCLUÍDO (Fase 1)

**Status:** corrigido, reauditado e validado. Os oito modos são preservados no display list e no trace: 0/4 fill, 1/5 stroke, 2/6 fill+stroke e 3/7 sem pintura. O modo 3 mantém a camada textual sem acender pixels. Modos 4–7 extraem e emitem `PDF_CLIP_TEXT_VISUAL`, ligado ao objeto de texto afetado, porque adicionar o contorno textual ao clipping path ainda não é suportado.

**Impacto:** 2 arquivos (`grpr.pdf`, `grpr (1).pdf`), com `PDF text rendering mode 3`.

**Sintoma:** `capabilities.text = false` no documento inteiro.

**Causa raiz:** `crates/docsight-pdf/src/content.rs:566` — qualquer modo diferente de 0 vira `UnsupportedFeature`.

**Por que é grave:** o modo 3 é *texto invisível*, ou seja, camada de OCR sobre página escaneada. É exatamente o texto que se quer extrair, e o engine se recusa a extrair porque ele não seria pintado. Modos 1 e 2 (stroke, fill+stroke) também são irrelevantes para extração.

**Direção:** modos 0–3 devem ser aceitos para extração; 4–7 (clipping) podem rebaixar `visual` com diagnóstico. O modo afeta pintura, não conteúdo.

---

## A5 — `Tz` negativo é fatal — CONCLUÍDO (Fase 1)

**Status:** corrigido, reauditado e validado. Sinal negativo é aceito no bbox por min/max e preservado como `mirrored_x` no display list/trace; bitmap e outline são pintados na orientação horizontal efetiva, incluindo o cancelamento por um CTM também espelhado. `Tz == 0` vira no-op como A3. Tamanhos de fonte negativos também extraem com `font_size` público absoluto, mas continuam emitindo `PDF_NEGATIVE_FONT_SIZE_VISUAL`, agora ligado ao objeto afetado, porque o raster axis-aligned não reproduz toda a transformação vertical assinada. No corpus histórico, `Carne_Contrato_83876_0.pdf` passou a inspecionar com `text=true`, 5 parágrafos e 6 tabelas.

**Impacto:** 1 arquivo (`Carne_Contrato_83876_0.pdf`, boleto bancário com `-100 Tz`).

**Sintoma:** exit `11`, `malformed document: text horizontal scale must be positive`.

**Causa raiz:** `crates/docsight-pdf/src/content.rs:552` — rejeita `value <= 0`.

**Direção:** `Tz` negativo é PDF válido (texto espelhado horizontalmente). Aceitar o sinal na matriz de texto e derivar o bbox pelo mínimo/máximo dos cantos (o código de `content.rs:858` já faz isso). `Tz == 0` colapsa o avanço: tratar como A3 (no-op), não como documento malformado.

---

## A6 — Soft masks fatais — CONCLUÍDO (Fase 3)

**Status:** corrigido, reauditado e validado. `SMask`, espaço de cor não calibrado, padrão e blend não Normal só geram diagnóstico quando uma operação realmente pinta sob esse estado. Na Document IR, cada aviso recebe o ID determinístico dos blocos intersectados; quando não existe objeto semântico correspondente, o escopo permanece explicitamente na página. Um `ExtGState` declarado e não usado não produz falso positivo. Os códigos continuam `PDF_SOFT_MASK_IGNORED`, `PDF_COLOR_SPACE_UNSUPPORTED`, `PDF_PATTERN_PAINT_UNSUPPORTED` e `PDF_BLEND_MODE_UNSUPPORTED`; `AIS`/`OP` true e sombreamentos `sh` continuam fail-closed. No corpus Windows histórico, os três livros mantiveram 1573, 5068 e 1319 parágrafos, respectivamente.

**Impacto:** 3 arquivos, e são justamente os livros grandes: `backend.pdf` (16 MB), `frontend.pdf` (53 MB), `uxsecrets.pdf` (37 MB).

**Causa raiz:** `crates/docsight-pdf/src/content.rs:2101`.

**Direção:** SMask é transparência: afeta pintura, não texto nem estrutura. Ignorar a máscara para extração e rebaixar `visual.fidelity` com diagnóstico (`PDF_SOFT_MASK_IGNORED`, objetos afetados = os pintados sob a máscara).

---

## A7 — DOCX: target OPC absoluto tratado como path traversal — CONCLUÍDO (Fase 1)

**Status:** corrigido e validado. `/x/y` agora resolve para a raiz do pacote; `..` além da raiz, drive letters, UNC, `\` e `:` continuam rejeitados. Imagem quebrada gera `DOCX_IMAGE_UNRESOLVED` + placeholder, sem invalidar texto/estrutura. Partes `media/` na raiz passaram a ser endereçadas.

**Impacto:** 2 arquivos rejeitados com exit `11`.

**Sintoma:** `malformed document: relationship Raccb0e24345c4574 has an invalid internal target`.

**Evidência:** em `word/_rels/document.xml.rels`:

```xml
<Relationship Type=".../image" Target="/media/image.png" Id="Raccb0e24345c4574" />
```

A barra inicial em OPC significa "raiz do pacote", não "raiz do filesystem". A parte existe no zip — `zipfile.namelist()` confirma `media/image.png` presente nos dois arquivos.

**Arquivos:** `967d99ab-1741-47f3-bb21-4c337405f19a.docx`, `bf69a5a6-ac25-4fd2-b544-c42d91e89fcb.docx`.

**Direção:** na normalização de caminho OPC, resolver `/x/y` como caminho absoluto **do pacote** (equivalente à parte `x/y`) e manter a rejeição para `..`, drive letters, UNC e barras invertidas. Além disso, uma relação de **imagem** quebrada não deveria invalidar o documento: cabe diagnóstico e recurso marcado como não resolvido.

---

## A8 — Form XObjects não são percorridos: zero texto

**Impacto:** 8 arquivos parseiam com sucesso e devolvem **nenhum** bloco de texto.

**Sintoma:** `paragraphs: 0`, `headings: 0`, apenas figuras; warning `PDF_XOBJECT_PLACEHOLDER: page N contains XObject content represented as a figure placeholder`. `docsight text` não imprime nada.

| Arquivo | páginas | figuras | linhas de texto |
| --- | --- | --- | --- |
| `copel.pdf` / `copel2.pdf` / `copel3.pdf` | 1 | 3 | 0 |
| `lattes.pdf` | 3 | 3 | 0 |
| `uel.pdf` | 1 | 5 | 0 |
| `Cancelamento_Fies.pdf` | 2 | 9 | 0 |
| `declaração-não-vinculo.pdf` | 1 | 2 | 0 |
| `DevBox - Cursos e Certificados.pdf` | 14 | 0 | 0 (tudo virou tabela) |

**Efeito colateral no `diff`:** comparar `copel.pdf` com `copel2.pdf` (duas contas de luz diferentes) produz `+2 figuras / -2 figuras, 0 parágrafos modificados`, mais dois `DIFF_LINEAGE_AMBIGUOUS`. É ruído: nenhuma diferença real de valor ou data aparece.

**Direção:** executar o content stream do Form XObject no contexto do CTM/recursos do pai, com limite de profundidade e proteção contra recursão (`/Do` circular). Manter o placeholder apenas para XObjects de imagem.

---

## A9 — Texto de célula de tabela sai destruído — CONCLUÍDO (Fase 2)

**Status:** corrigido e validado. Junção ruled e alignment unificada com a lógica do parágrafo (`gap >= 2.0pt` → espaço, sem espaço entre glifos contíguos); colunas de alignment particionadas por `x0` nearest sem sobreposição, de modo que um run nunca é cortado nem duplicado.

**Impacto:** todas as tabelas inferidas de PDF. Afeta `table`, `tables`, `text`, `query`, `context --find` e `resolve`.

**Sintoma medido:**

`table Invoice-KOI2MLFW-0002.pdf tbl_9fd371... --format markdown`:
```
| Q       | Q t y U n i t   p r | r i c e A m o u | u n t |
| . 0 0   | 1 $ 9 . 0           | . 0 0 $ 9 . 0   | . 0 0 |
```

`table Receipt-2936-0480.pdf ... --format csv`:
```
$ 9 .,. 0 0
p a i d $ 9 . 0,. 0 0
```

Dois padrões distintos, provavelmente causas distintas:

1. caracteres separados por espaço — a montagem junta glifos individuais com separador em vez de decidir por distância entre avanços;
2. caractere da borda **duplicado** entre colunas (`A m o u` / `u n t`, `$ 9 .` / `. 0 0`) — a fronteira de coluna corta dentro do run e o caractere aparece dos dois lados.

**Prova de que o dado está correto:** `crop --object tbl_9fd371...` do mesmo objeto renderiza legível e correto — "Description | Qty | Unit price | Amount", "PRO Subscription (monthly) (1, $9.00) | 1 | $9.00 | $9.00". A geometria e os glifos estão certos; quebra é a montagem span → célula.

**Consequência direta:** `docsight --agent context Invoice-KOI2MLFW-0002.pdf --find "Subtotal"` devolve `status: no_match` num documento onde "Subtotal" está visível no render.

**Direção:** montar o texto da célula a partir dos runs originais (mesma lógica que o parágrafo usa, que sai limpo), com quebra de coluna baseada em intervalo entre bbox de runs, sem cortar dentro de um run e sem inserir espaço entre glifos contíguos.

---

## A10 — Confiança descalibrada em tabela inferida — CONCLUÍDO (Fase 2)

**Status:** corrigido e validado. Penalidades determinísticas em alignment: `-0.15` para `cols > 4*rows` ou `cols >= 12`, `-0.10` para largura mínima `< 20pt`, `-0.10` para coluna `>= 90%` vazia ou `> 50%` de células vazias, piso `0.35`. A tabela `3x22` cai para `< 0.70` e a regra existente de `review` (`confidence < 0.70`) passa a marcar revisão.

**Sintoma:** em `FICHA_DE_INFORMACAO_MEDICA_E_DADOS_CADASTRAIS.pdf`, página 2, tabela `3 x 22` com `confidence: 0.934`, `detector: alignment`, `review: false`.

Vinte e duas colunas numa ficha cadastral é quase certamente sobre-segmentação, e o objeto se apresenta como confiável e sem necessidade de revisão.

**Direção:** incluir na pontuação sinais de implausibilidade estrutural (razão colunas/linhas, colunas com célula vazia em quase todas as linhas, largura de coluna abaixo de um mínimo) e ligar `review: true` quando dispararem.

---

## A11 — `resolve` diz `ambiguous` com um único candidato, e casa a palavra errada — CONCLUÍDO (Fase 2)

**Status:** corrigido, reauditado e validado. `ambiguous` exige dois ou mais candidatos competitivos. Se o melhor score ficar abaixo do limiar, o resultado é `low_confidence`, mesmo com candidato único, e nenhum contexto é escolhido silenciosamente. `resolved` fica reservado a evidência acima do limiar e sem empate dentro da margem. Frase completa continua superando palavra parcial pelo scoring existente, restaurado pela correção de A9.

**Sintoma:** `docsight --agent resolve Invoice-KOI2MLFW-0002.pdf --text "Amount due"` retorna `status: "ambiguous"` com `candidates.len() == 1`. O candidato é o parágrafo `Date of issue January 1, 2026 Date due …`, casado pela palavra "due"; o "Amount due" real está dentro da tabela (ver A9).

**Direção:** `ambiguous` só faz sentido com dois ou mais candidatos competitivos; com um só, `resolved` (ou `low_confidence`) com o motivo. E casamento por token completo deveria pontuar acima de casamento parcial de palavra.

---

## A12 — PNG praticamente sem compressão — CONCLUÍDO (Fase 3)

**Status:** corrigido e validado. Novo `docsight_core::encode_png` único (elimina 2 implementações idênticas em `docsight-pdf` e `docsight-render`; `docsight-diff` já reutilizava o do render): filtro por scanline (None/Sub/Up/Average/Paeth pela menor soma absoluta) + deflate nível 6 via `flate2/zlib-rs`. `DevBox` p.1/144dpi: 6.019.139 → 718.107 bytes (~8,4×), `FLEVEL=2`, filtros usados Up/Paeth/Sub/None/Average. Pixels idênticos (codec lossless, mesma entrada RGB; geometria dos goldens inalterada). Goldens atualizados deliberadamente (só `render_page1_sha256`) + `m15-evidence-engine.json` (hashes de raster/trace).

**Sintoma:** uma página de `DevBox - Cursos e Certificados.pdf` a 144 dpi sai com **6.019.139 bytes**.

**Análise do arquivo gerado:**

- `IHDR 1191x1684 depth=8 color_type=2` (RGB), sem entrelace;
- filtro de scanline **tipo 0 (None) nas 1684 linhas**;
- cabeçalho zlib `CMF=78 FLG=01` → `FLEVEL=0`, nível "fastest";
- IDAT = 6.019.082 bytes para 8.024.260 bytes crus: razão 0,75.

Recomprimindo **os mesmos bytes** em nível 6: 461.627 bytes — 13× menor, sem mudar um pixel. Com filtro `Up`/`Paeth` cai bem mais.

**Impacto:** multiplica em `bundle`, contact sheet e `diff --visual`.

**Direção:** escolher filtro por scanline (heurística padrão de soma mínima de valores absolutos) e subir o nível de deflate. Como isso muda bytes de saída, atualizar goldens visuais deliberadamente e registrar a mudança de fingerprint de render.

---

## A13 — Sem cache entre chamadas

**Medição (release, mesmo arquivo, chamadas independentes):**

| Documento | `inspect` | `text` | `tables` |
| --- | --- | --- | --- |
| `DevBox - Cursos e Certificados.pdf` (14 p) | 1638 ms | 1606 ms | 1618 ms |
| `Projeto_HELIX_Especificacao.docx` (15 p) | 153 ms | 149 ms | 187 ms |

Cada comando reparseia o documento do zero. Um fluxo de agente comum (`inspect` → `tables` → `table` → `crop` → `evidence`) paga ~1,6 s cinco vezes para o mesmo arquivo imutável.

Outras medições: render de página PDF ~706 ms; 14 páginas em 7,7 s; arquivos grandes bloqueados falham rápido (53 MB em 216 ms); overhead do `--sandbox` no Windows ≈ 70 ms por execução, com saída byte-idêntica à direta.

**Direção:** é o `docsight-cache` já previsto na arquitetura — endereçado por digest do arquivo + versão do engine + fingerprint de fontes + opções, escrita atômica, invalidação explícita.

---

## A14 — Contrato do agente incompleto — CONCLUÍDO (Fase 3)

**Status:** corrigido e validado. 8 schemas publicados (`outline/text/tables/table/page/images/links/fingerprint-result.json`) e ligados em `capabilities`; novo `result_root` por comando (`headings`/`blocks`/`tables`/`images`/`links`, nulo quando o resultado é o próprio objeto); novo `error_channel: "stderr"` declarando stdout vazio em erro (também no modo humano). Teste de contrato estendido.

1. **Oito comandos sem `result_schema`**, justamente os mais básicos: `outline`, `text`, `tables`, `table`, `page`, `images`, `links`, `fingerprint`. O `capabilities` promete o schema "quando existe um schema público dedicado" — para esses, não existe.
2. **Nomes de campo do resultado não são descobríveis:** `overview` devolve `landmarks`, `text` devolve `blocks`, o token de continuação é `limits.continuation_token`. Descobri cada um errando primeiro; um agente sem tentativa e erro não acerta.
3. **Em modo agente, o envelope de erro sai em `stderr` com `stdout` vazio.** Medido: `stdout` 0 bytes, `stderr` 224 bytes. É coerente com "stdout é API", mas não está declarado em `capabilities` — que anuncia `error_schema` sem dizer o canal.

**Direção:** publicar schema para os oito comandos e declarar em `capabilities` (a) o canal do envelope de erro e (b) o nome do campo raiz de cada resultado.

---

## A15 — Erro sem localização — CONCLUÍDO (Fase 3)

**Status:** corrigido, reauditado e validado. `MALFORMED_DOCUMENT` originado no content stream carrega `ErrorLocation` tipado com `page`, `object` e `offset`; `operator` também é preenchido quando o parser já alcançou um operador, e é omitido em erro léxico anterior a ele. O offset é local ao stream indicado, inclusive quando `/Contents` é um array; o envelope `docsight.agent/v2` expõe `error.location`, e o schema público foi atualizado. Exit code 11 e código `MALFORMED_DOCUMENT` permanecem estáveis.

`MALFORMED_DOCUMENT: malformed document: text operator produced invalid geometry` não diz página, offset no content stream, número do objeto nem operador. Para quem tem o arquivo abrindo normalmente em qualquer visualizador, é indepurável — e foi preciso descomprimir os streams por fora para descobrir a causa real (A3).

**Direção:** anexar página, índice do objeto e operador ao contexto tipado do erro. O AGENTS.md já exige "contexto suficiente para ação".

---

## A16 — `verify` devolve caminho absoluto local — CONCLUÍDO (Fase 3)

**Status:** corrigido e validado. Campo `bundle_path` substituído por `bundle_name` (só o nome do arquivo); digest continua como identidade. Schema `verify-result.json` atualizado sem alias legado.

`docsight --agent verify <bundle>.dse` inclui `result.bundle_path` com o caminho absoluto local do arquivo. Contraria a regra do próprio AGENTS.md de não incluir caminhos absolutos locais em saídas determinísticas.

**Direção:** remover o campo ou reduzi-lo ao nome do bundle, mantendo o digest como identidade.

---

## A17 — Ruído em `text` NDJSON — CONCLUÍDO (Fase 3)

**Status:** decidido e validado. Registros com texto vazio/só-espaço saíram do `text` (JSON, NDJSON e humano); seguem em `overview`/`page`/`table`. Fixture de validação atualizada de 50 para 49 blocos com asserção de nenhum vazio.

O stream incluía blocos vazios ou só com espaço (`{"kind":"paragraph","text":" "}`, `{"kind":"figure","text":""}`), que o consumidor precisava filtrar.

---

## O que está correto e não deve regredir

- **Render fiel.** `Invoice-KOI2MLFW-0002.pdf` a 96 dpi saiu com layout, pesos de fonte e acentuação corretos (`Inácio`, `Cambé-Paraná`), com o logo honestamente marcado como placeholder.
- **`crop --object`** recorta exatamente o bbox do objeto (verificado visualmente na tabela de itens da fatura).
- **Exit codes** batem com a especificação em todos os casos provocados: `10` (jpg, saz), `11` (malformed), `12` (encriptado), `20` (recurso não suportado), `40` (arquivo inexistente).
- **Bounded output:** `--budget 2kb` devolveu 2020 bytes com `projection.selected_profile: "rich"`; continuation token avança corretamente (5 → 10 de 49 itens).
- **Cadeia de evidência:** `evidence` → `bundle --include-crop` → `verify` funcionou fim a fim num documento real.
- **`hit`** acertou o objeto sob o ponto (`--point 60,40` → heading "Invoice").
- **Sandbox** não alterou a saída (byte-idêntica) e custou ~70 ms por execução no Windows.
- **Encriptado** é recusado com `12` e mensagem clara, sem tentativa de contornar.

## Anexo — classificação histórica do corpus Windows antes das correções

**Usáveis (10):** `2176230.pdf`, `Invoice-KOI2MLFW-0002.pdf`, `Receipt-2936-0480.pdf`, `Profile.pdf`, `Projudi_2018.pdf`, `Veil_of_NTherya_Design.pdf`, `FICHA_DE_INFORMACAO_MEDICA_E_DADOS_CADASTRAIS.pdf`, `Considerações finais….docx`, `Projeto_DOCSIGHT_Especificacao.docx`, `Projeto_HELIX_Especificacao.docx`.

**Rejeitados (9):** `01-20251663927108.pdf` (encriptado, correto), `Entre+na+plataforma.pdf` (encriptado, correto), `boletim.pdf` (A3), `arquivo (1).pdf` (A3), `Imposto_Casa.pdf` (A3), `f116e911-….pdf` (A3), `Carne_Contrato_83876_0.pdf` (A5), `967d99ab-….docx` (A7), `bf69a5a6-….docx` (A7).

**Sem capacidade (16):** 4 × A1 (xref streams), 1 × A1b (incremental), 6 × A2 (`SA`), 3 × A6 (soft masks), 2 × A4 (`Tr 3`).

**Sem texto (8):** `copel.pdf`, `copel2.pdf`, `copel3.pdf`, `lattes.pdf`, `uel.pdf`, `Cancelamento_Fies.pdf`, `declaração-não-vinculo.pdf`, `DevBox - Cursos e Certificados.pdf` — todos A8.

## Anexo — reprodução histórica rápida no corpus Windows

```
cd C:\Users\crist\Downloads

# A1
docsight --agent inspect "9781801073394-MOBILE_APP_REVERSE_ENGINEERING.pdf"
# A2
docsight --agent inspect CV_GUSTAVO_MARTINS.pdf
# A3 (um único operador vazio derruba o arquivo)
docsight inspect boletim.pdf
# A4
docsight --agent inspect grpr.pdf
# A5
docsight --agent inspect Carne_Contrato_83876_0.pdf
# A7
docsight --agent inspect 967d99ab-1741-47f3-bb21-4c337405f19a.docx
# A8
docsight text copel.pdf                  # nenhuma saída
docsight --agent diff copel.pdf copel2.pdf
# A9
docsight table Invoice-KOI2MLFW-0002.pdf tbl_9fd371cb2e6d77e07dee363c0a380d54 --format markdown
docsight crop  Invoice-KOI2MLFW-0002.pdf --object tbl_9fd371cb2e6d77e07dee363c0a380d54 --out crop.png --dpi 144
docsight --agent context Invoice-KOI2MLFW-0002.pdf --find "Subtotal"
# A11
docsight --agent resolve Invoice-KOI2MLFW-0002.pdf --text "Amount due"
# A12
docsight render "DevBox - Cursos e Certificados.pdf" --page 1 --out p1.png --dpi 144
# A13
docsight --agent inspect "DevBox - Cursos e Certificados.pdf"
```
