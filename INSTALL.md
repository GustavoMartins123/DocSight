# Install a DocSight binary

Binary releases are built by the release workflow. A source checkout alone does
not mean these binaries have been published. Until a maintainer publishes a
release, use the source-build instructions in README.md.

## Supported release targets

| Target | Build and test environment | Archive |
| --- | --- | --- |
| Windows x64 | Windows Server 2022, MSVC, static C runtime | `docsight-VERSION-x86_64-pc-windows-msvc.zip` |
| Linux x64 | Ubuntu 24.04, GNU libc | `docsight-VERSION-x86_64-unknown-linux-gnu.zip` |
| Linux ARM64 | Ubuntu 24.04 ARM64, GNU libc | `docsight-VERSION-aarch64-unknown-linux-gnu.zip` |
| macOS Intel | macOS 15 | `docsight-VERSION-x86_64-apple-darwin.zip` |
| macOS Apple Silicon | macOS 15 | `docsight-VERSION-aarch64-apple-darwin.zip` |

These are the tested operating-system baselines, not a promise about every Linux
distribution or older macOS version. Alpine/musl and 32-bit systems are not release
targets. Rust, Python, Office and a network connection are not required to run the
extracted binary. Python is used only by maintainer automation.

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
not authenticate the publisher. Current artifacts are not code-signed or
notarized. Do not disable system security checks to work around this limitation.
Signing and macOS notarization require maintainer credentials and remain a
release-readiness item.

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
copy only the executable when distributing the tool onward: retain the licenses,
third-party notices, schemas and documentation shipped beside it.

The quickstart in README.md covers discovery, navigation, evidence and password
files. Unsupported document features fail explicitly or carry the documented
fidelity diagnostics.

## Upgrade and uninstall

Extract a new release into a new directory and check its version and capabilities
before replacing the PATH entry. Keep the previous directory until your scripts
pass their smoke tests. Uninstall by removing the PATH entry and the extracted
directory. DocSight does not install a service or schedule telemetry.
