# Releasing OMGB

The release workflow runs when a `v*` tag is pushed. It builds signed, attestable release archives for Linux x86_64/ARM64, macOS Intel/Apple Silicon, and Windows x86_64, then creates a **draft** GitHub Release. Drafts are deliberately invisible to `omgb update`.

Before creating a release, run the normal Rust and mobile checks in their respective repositories and ensure the Cargo version matches the intended `vMAJOR.MINOR.PATCH` tag.

```bash
git tag -a vMAJOR.MINOR.PATCH -m "vMAJOR.MINOR.PATCH"
git push origin vMAJOR.MINOR.PATCH
```

After the workflow completes, inspect the draft release. It must contain all five `omgb-*.tar.gz` archives, AMD64/ARM64 `omgb_*.deb` and `omgb-*.rpm` packages, the x64 `omgb-*.msi`, `install.sh`, `install.ps1`, the generated `omgb.rb` Homebrew formula, `checksums-sha256.txt`, `checksums-sha512.txt`, and one SPDX JSON SBOM per target. Stable releases also include the three schema-1.12 WinGet manifests under `winget/`. The checksum manifests must cover both installers, all native packages, the formula, and all archives. Download an archive and verify its provenance before publishing:

```bash
gh attestation verify omgb-x86_64-unknown-linux-gnu.tar.gz \
  --repo josepha-mayo/oh-my-grok-build \
  --signer-workflow josepha-mayo/oh-my-grok-build/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

Run both installers in disposable platform-appropriate directories with an explicit tag, and confirm that the installed `omgb doctor` resolves the packaged sibling `plugin` directory. Inspect each Debian package with `dpkg-deb --info` and `dpkg-deb --contents`, each RPM with `rpm -qip` and `rpm -qlp`, and the MSI with `msiextract -l`; then install them in disposable Debian/Ubuntu, Fedora-compatible, and Windows environments and run `omgb doctor`. Confirm MSI uninstall removes the PATH entry. Run `ruby -c omgb.rb` and a disposable `brew install --formula ./omgb.rb` on macOS. Confirm the checksum, artifact names, generated release notes, and attestation verification result. Then publish the draft. Only published releases are considered by `omgb update --check`, `omgb update --apply`, and an installer invoked without an explicit version.

For a stable release, validate the generated WinGet manifests with `winget validate`, install from the published MSI URL in a disposable Windows environment, and only then submit the manifest directory to `microsoft/winget-pkgs`. The community repository requires an immutable public installer URL and may require the submitter to complete Microsoft's CLA; do not submit manifests that still point at a draft or replace the MSI after submission.

If validation fails, leave the release unpublished and correct the issue with a new version tag. Do not replace published archives: release attestations and checksums are tied to the exact artifact bytes.
