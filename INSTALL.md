# Install a DocSight binary

Binary releases are built by the release workflow. A source checkout alone does
not mean these binaries have been published. Until a maintainer publishes a
release, use the source-build instructions in README.md.

## Configured release targets

| Target | Build and test environment | Archive |
| --- | --- | --- |
| Windows x64 | Windows Server 2022, MSVC, static C runtime | `docsight-VERSION-x86_64-pc-windows-msvc.zip` |
| Linux x64 | Ubuntu 24.04, GNU libc | `docsight-VERSION-x86_64-unknown-linux-gnu.zip` |
| Linux ARM64 | Ubuntu 24.04 ARM64, GNU libc | `docsight-VERSION-aarch64-unknown-linux-gnu.zip` |
| macOS Intel | macOS 15 | `docsight-VERSION-x86_64-apple-darwin.zip` |
| macOS Apple Silicon | macOS 15 | `docsight-VERSION-aarch64-apple-darwin.zip` |

These are the configured build and required native-test baselines, not a record
that those tests have run. A candidate must pass its native checks before these
targets can be called validated. They do not promise support for every Linux
distribution or older macOS version. Alpine/musl and 32-bit systems are not release
targets. Rust, Office and a network connection are not required to run the
extracted binaries. Maintenance and candidate preparation use the Rust `xtask`
crate. Each archive contains both `docsight` and `docsight-worker` (with `.exe`
on Windows), plus schemas, examples, documentation and license resources.

## Verify before extracting

Obtain the archive and its `.sha256` file from the same maintainer-published
release. Check the checksum in the download directory before extracting.

Linux:

```sh
sha256sum -c docsight-VERSION-x86_64-unknown-linux-gnu.zip.sha256
unzip docsight-VERSION-x86_64-unknown-linux-gnu.zip
```

macOS:

```sh
shasum -a 256 -c docsight-VERSION-aarch64-apple-darwin.zip.sha256
unzip docsight-VERSION-aarch64-apple-darwin.zip
```

Windows PowerShell:

```powershell
$archive = 'docsight-VERSION-x86_64-pc-windows-msvc.zip'
$expected = (Get-Content "$archive.sha256" -Raw).Split(' ')[0]
$actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw 'Checksum mismatch; do not run this archive.' }
Expand-Archive $archive
```

Choose the archive for your architecture. Checksums detect corruption; they do
not authenticate the publisher.

## Verify the publisher

The release pipeline can attach three proofs of origin, each enabled only once
the maintainer has the corresponding credentials. Today's candidates carry none
of them (RELEASE.md lists what is pending), so these commands fail on them. Do
not disable system security checks to run an archive whose publisher you cannot
verify.

Build provenance, on every platform, with the GitHub CLI signed in to an
account that can read the repository:

```sh
gh attestation verify docsight-VERSION-TARGET.zip --repo GustavoMartins123/DocSight --signer-workflow GustavoMartins123/DocSight/.github/workflows/release.yml
```

A successful result shows that GitHub's release workflow for this repository
built that exact archive on a GitHub-hosted runner.

macOS signature and notarization, after extracting:

```sh
codesign --verify --strict --verbose=2 docsight
spctl --assess --type install --verbose=2 docsight
```

The second command must report `source=Notarized Developer ID` and the
maintainer's Team ID in its `origin` line.

Windows Authenticode signature, after extracting:

```powershell
Get-AuthenticodeSignature .\docsight.exe | Format-List Status, SignerCertificate
```

`Status` must be `Valid` and the signer subject must name the maintainer.
Linux has no platform code signature; use the build provenance check.

## Run offline

Open the extracted directory and run `./docsight --version` on Linux/macOS or
`.\docsight.exe --version` on Windows. Then run:

```sh
./docsight --agent capabilities
./docsight --agent inspect examples/sample_headings.docx
./docsight --agent inspect examples/sample_semantic.pdf
```

On Windows use `.\docsight.exe` instead of `./docsight`. Add the extracted
directory to your PATH, or keep calling the executable by its full path. Do not
copy only the CLI when distributing the tool onward: retain the worker, licenses,
third-party notices, schemas and documentation shipped beside it.

The quickstart in README.md covers discovery, navigation, evidence and password
files. Unsupported document features fail explicitly or carry the documented
fidelity diagnostics.

## Upgrade, roll back and uninstall

Keep each release in its own directory, named after the archive, and select the
active one through your PATH entry. DocSight writes only to the output paths
and the `--cache-dir` you pass, plus a temporary directory that `--sandbox`
removes when it finishes. It installs no service and keeps no configuration, so
switching directories switches versions completely.

```text
tools/
  docsight-0.1.4-x86_64-unknown-linux-gnu/    previous release, left untouched
  docsight-0.1.5-x86_64-unknown-linux-gnu/    new release, extracted beside it
```

1. Update: verify and extract the new archive beside the current one, run its
   `--version` and `--agent capabilities` and your own smoke tests with its full
   path, then point your PATH entry at the new directory.
2. Roll back: point the PATH entry back at the previous directory. Its files
   were never modified, so it behaves exactly as before the update.
3. Uninstall: remove the PATH entry and delete the release directory.

`--agent capabilities` states the protocol, commands, error codes with their exit
codes and document formats of each version. A release that removes or changes
any of them, or removes a published schema, changes the major version (the
minor version while the major version is 0); scripts pinned to an older version
should compare capabilities before switching.

Maintainers exercise exactly this procedure for every pair of consecutive
releases with `cargo xtask release lifecycle`, described in RELEASE.md.
