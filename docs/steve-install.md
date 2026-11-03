# Install a Steve binary package

Steve's native packages contain the CLI, code-mode host, executor, ripgrep and
platform sandbox helpers. Rust, Python and a local V8 build are not needed to
run a downloaded package.

## Download

Open [Steve binary build](https://github.com/handley-lab/steve/actions/workflows/steve-binary-build.yml)
and choose a successful run for the commit you want. Download the artifact
matching your operating system and processor while signed into GitHub.

| Platform | Target in the artifact name | Package inside the artifact |
| --- | --- | --- |
| Linux x86-64, glibc | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux ARM64, glibc | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows x86-64 | `x86_64-pc-windows-msvc` | `.zip` |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `.zip` |

The [first successful six-platform run](https://github.com/handley-lab/steve/actions/runs/41686805704)
built commit `c5ba0407817495d1c44fd031e0b6ef1024d978a5`. Linux packages were
built on Ubuntu 22.04; macOS packages on macOS 15; Windows packages on Windows
2022 (x86-64) and Windows 11 (ARM64). This does not certify other OS versions.

GitHub downloads an outer artifact ZIP. Extract that first: it contains the
package archive, `SHA256SUMS`, `build-info.txt`, `v8.sha256`, `LICENSE` and
`NOTICE`. Artifacts expire after 14 days; they are CI downloads, not permanent
release URLs. Builds run after changes reach `main`, or by manual dispatch.

## Linux and macOS

In the directory containing the extracted artifact, verify the package:

```sh
# Linux
sha256sum --check SHA256SUMS

# macOS
shasum -a 256 --check SHA256SUMS
```

Extract into a new directory. For example, on x86-64 Linux:

```sh
mkdir -p "$HOME/.local/opt/steve-c5ba040781"
tar -xzf steve-x86_64-unknown-linux-gnu.tar.gz \
  -C "$HOME/.local/opt/steve-c5ba040781"
"$HOME/.local/opt/steve-c5ba040781/bin/steve" --version
export PATH="$HOME/.local/opt/steve-c5ba040781/bin:$PATH"
steve
```

Use the target and commit you downloaded in the archive and directory names.
To keep the command available in new terminals, add the corresponding PATH
assignment to your shell's startup file.

## Windows

After extracting the outer artifact ZIP, verify the inner package in PowerShell:

```powershell
$archive = 'steve-x86_64-pc-windows-msvc.zip'
$expected = ((Get-Content .\SHA256SUMS).Trim() -split '\s+')[0]
$actual = (Get-FileHash $archive -Algorithm SHA256).Hash
if ($actual -ne $expected) { throw 'Package checksum mismatch' }
$destination = Join-Path $env:LOCALAPPDATA 'Steve\c5ba040781'
Expand-Archive -Path $archive -DestinationPath $destination
& "$destination\bin\steve.exe" --version
$env:PATH = "$destination\bin;$env:PATH"
steve
```

Use the ARM64 archive on Windows ARM64. Add the package's `bin` directory to
your user PATH through Windows Environment Variables for future terminals.

## Configuration and package layout

Keep the complete extracted directory together. Copying only `bin/steve` or
`bin/steve.exe` loses the sibling hosts, `steve-resources`, `steve-path` and
package manifest used to locate bundled tools.

Steve reads `~/.steve/config.toml`, or the home selected by `STEVE_HOME`.
Choose a provider, its endpoint and a model explicitly; see
[configuration](../README.md#separate-configuration). An existing Codex
configuration is not imported automatically.

These packages are unsigned and macOS packages are not notarized. The checksum
verifies the downloaded archive against the run's manifest; it is not a release
signature. The code-mode host uses the pinned, checksum-verified V8 archive and
bindings published by `openai/codex`. The artifact records the source commit,
target, Rust toolchain versions and V8 download source. See [V8 provenance](../third_party/v8/README.md).

CI extracts the package and runs CLI version, help and shell-completion checks,
the bundled ripgrep version check, and the linked V8 sandbox test. Linux also
checks the packaged sandbox helper's digest. A successful package job does not
certify a real provider session or the complete test suite.
