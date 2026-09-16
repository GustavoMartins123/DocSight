# Performance and scale

DocSight keeps a versioned Linux x86_64 release benchmark for the scale classes promised by DS-9. The benchmark is an engineering gate, not a claim that every host will have identical timings.

Run the same gate used by CI:

```bash
cargo run --release -p xtask -- benchmark --check
```

Write the measured report to a file while still emitting it on stdout:

```bash
cargo run --release -p xtask -- benchmark --output target/ds9-performance.json
```

The report has schema `docsight.performance-report/v1`. Each scenario runs three times in a fresh process. Timings use the median; peak memory uses the highest Linux `VmHWM` reading. The worker separately measures parsing, DOCX layout or PDF normalization, and end-to-end page-one render latency. Output size is the canonical serialized IR plus the rendered PNG. Throughput excludes rendering and is computed from parsing plus layout or normalization.

## Versioned scenarios

| Scenario | Scale represented |
| --- | --- |
| `docx_small` | Small checked-in DOCX fixture. |
| `docx_medium` | The project specification, with tens of pages and hundreds of semantic objects. |
| `docx_5000_blocks` | Deterministically generated DOCX with 5,000 paragraphs and more than 100 laid-out pages. |
| `pdf_1_page` | One-page PDF baseline. |
| `pdf_100_pages` | Deterministically generated 100-page PDF. |
| `pdf_1000_pages` | Deterministically generated 1,000-page PDF. |
| `pdf_large_image` | PDF carrying a 2048 by 2048 RGB image stream. |

The canonical ceilings and minimum throughput are in [`benchmarks/ds9-budgets.json`](benchmarks/ds9-budgets.json). They target Rust 1.96.0, release mode, Linux x86_64. CI rejects a scenario that exceeds its time, peak-memory or output-size ceiling, falls below its throughput floor, returns too few pages or objects, or violates the declared memory-growth ratios. Budget changes require review like any other contract; the benchmark never rewrites them.

## Practical boundaries

The benchmark demonstrates representative behavior inside the supported limits. It does not override input safety limits. `docsight --agent capabilities` is authoritative for maximum document bytes, XML nodes, package expansion, PDF objects, pages, decoded image pixels, raster pixels and output budgets.

Peak-memory regression enforcement is intentionally pinned to Linux x86_64 so results stay comparable. Product code and the functional test suite remain cross-platform. Other hosts can use the normal commands, but the benchmark command fails explicitly instead of publishing incomparable memory numbers.
