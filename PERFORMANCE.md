# Performance and scale

DocSight keeps a versioned Linux x86_64 release benchmark for the promised scale classes. The benchmark is an engineering gate, not a claim that every host will have identical timings.

Run the same gate used by CI:

```bash
cargo run --locked --release -p xtask --bin xtask -- benchmark --check
```

Write the measured report to a file while still emitting it on stdout:

```bash
cargo run --locked --release -p xtask --bin xtask -- benchmark --output target/ds9-performance.json
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

## Operation matrix

`benchmark operations` measures the agent operations clients actually invoke, process by process, instead of only engine internals:

```bash
cargo run --locked --release -p xtask --bin xtask -- benchmark operations --check
```

The matrix covers 25 operations over small DOCX/PDF fixtures, the specification document, a paired and self diff, page-one renders and a typed-error probe. Cacheable operations run both cold (no cache directory) and warm (reused `--cache-dir`); `diff`, `render`, `fingerprint` and the error probe run cold only because they re-ingest or never touch the IR cache. Every sample is repeated: cold iterations must be byte-identical, warm output must equal cold output, render artifacts must keep their digest, and the error probe must keep its exit code and envelope. The report has schema `docsight.operation-report/v1`.

Ceilings live in [`benchmarks/ds18-operations.json`](benchmarks/ds18-operations.json) with reference `windows/x86_64/release`. Wall-clock budgets carry about 100% headroom over the first measured medians and are preliminary; stdout byte counts are exact because agent output is deterministic, except for render entries whose paths embed the scratch directory. Peak-memory budgets are null where the host cannot measure them and are enforced only when a sample carries a reading. On a host that differs from the reference, `--check` still enforces determinism, exit codes, error envelopes and stdout byte counts, and skips wall-clock ceilings explicitly instead of comparing incomparable timings.

Measured baseline (Windows x64, release): every small-document operation completes in 30–110 ms with warm runs at or above cold runs, so process startup dominates and the cache pays off only once parsing or layout dominates. `diff_pair` is the heaviest entry at about 106 ms and 133 KB of JSON.

## Adversarial hotspot harness

`benchmark hotspots` measures deterministic large-scale workloads that the small operation matrix does not isolate:

```bash
cargo run --locked --release -p xtask --bin xtask -- benchmark hotspots --iterations 3 --output target/hotspots.json
```

The report uses schema `docsight.hotspot-report/v1`. Inputs are prepared before the measured function, every repetition must produce identical evidence bytes, and result hashes are the correctness oracle. Wall times are observational same-build comparisons; they are not portable budgets and do not fail on elapsed time.

The fixed cases are:

- `canonical_pages_10000` and `coverage_pages_10000` for page/block scans.
- `docx_keep_next_1000` and `docx_keep_next_10000` for long keep-with-next chains.
- `pdf_rulings_10000` and `pdf_unruled_10000` for dense table inference.
- `pdf_shared_resources_1000` for repeated indirect PDF resources.
- `pdf_vector_raster` for cubic paths, clipping and fallback glyph rasterization.
- `pdf_trace_and_proof` for trace and proof generation with and without a crop.
- `semantic_diff_1000` for rejected semantic alignment candidates.
- `unicode_find_512` for long case-insensitive Unicode literal search.

The first Windows x64 release baseline identifies `coverage_pages_10000` and `semantic_diff_1000` as the largest measured cases, followed by `pdf_vector_raster`; these values are stored only under ignored `target/` output and must be regenerated on the comparison host.

## Cost structure

Process startup is the dominant cost for small documents. `docsight --version` alone takes about 26 ms on Windows x64 release, and `inspect` of a 1.2 KB PDF takes about 27 ms, so the engine work is noise on tiny inputs. A 20 KB DOCX inspects in about 34 ms with roughly 78% startup share.

Inside the engine, page-one rasterization dominates: at 72 dpi it costs 17–35 ms against 0.1–18 ms of parsing plus layout or normalization, and it scales linearly with pixels (about 40 ns per pixel on both fixtures, converging to the same cost at 300 dpi for equal page areas). No operation hangs or blows up on the current corpus; the rasterizer stays pixel-linear with no superlinear stage found.

The IR cache halves a 29-page inspect from about 74 ms (miss) to about 38 ms (warm). Sandbox isolation on Windows adds about 100 ms per invocation (142 ms sandboxed against 34 ms plain for the same inspect), so agent workflows should batch work into fewer, larger calls. The five task scenarios run end to end in 212–632 ms with 1–4 sandboxed invocations each.

## Document IR cache

`--cache-dir` trades parsing and layout for hashing and validation. A hit still reads and hashes the document, hashes the running executable, verifies the entry digest, deserializes the IR and checks its canonical form, then runs the command on that IR. The cache therefore pays off when parsing or DOCX layout dominates, such as large or dense PDFs and long DOCX files, and can be slower than a plain run for small documents. A miss adds the same hashing plus serializing and atomically publishing the entry.

Under `--sandbox` the parent hashes the document and executable, validates the entry and hands it to the worker, which deserializes and validates it again inside the sandbox. The worker's process start and isolation setup are paid on both hits and misses, so the relative gain is smaller than for an unsandboxed hit. The cache is not part of the benchmark gate; measure it on representative documents with `cache stats` and repeated invocations before relying on a specific speedup.
