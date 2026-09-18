# Ground truth register

This directory holds one `<document-sha256>.json` record per document, using the
`docsight.ground-truth/v1` contract in `schemas/tooling/v2/evidence.json`. It is
empty: no document in the tracked corpus has reviewed ground truth yet.

`cargo xtask quality prepare` proposes a record from engine output and always
marks it `unreviewed`. A proposal is useful to detect drift between engine
versions, but it is not evidence that the engine is right. A record becomes
`reviewed` only when a person has checked every expected value against the
document, corrected the ones the engine got wrong, and recorded their stable
reviewer identifier together with the path and SHA-256 of a note under this
directory, for example `reviews/<document-sha256>.md`, that describes the
procedure, observations and limitations of the review.

Records for private or consented documents stay outside the repository; pass
their register with `--ground-truth`. See RELEASE.md, "Quality measurement by
document class", for the metrics and how reviewed and unreviewed records are
reported.
