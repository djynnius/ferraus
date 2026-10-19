# One-time release setup (maintainer)

Everything in the release pipeline is built; these steps need the maintainer's accounts and
must be done by a person. Afterwards, `git tag vX.Y.Z && git push origin vX.Y.Z` releases
everything, with a human approval before anything is published.

Run these from a clone of `djynnius/ferraus` with the GitHub CLI logged in (`gh auth login`).
Keep every secret file outside the repository and delete it after uploading.

**v0.1.0 (core channels):** sections 1, 2, 3, and the crates.io part of 7, with the repository
public (environment secrets and required reviewers need a public repository on the free plan).
macOS signing is off (`macos-sign = false`), and apt/dnf, containers, winget, and scoop are
opt-in through the repository variables `PUBLISH_APT_DNF`, `PUBLISH_CONTAINERS`, and
`PUBLISH_WINGET_SCOOP` (`gh variable set PUBLISH_APT_DNF --body true`) once their sections
are done.

## 1. The `release` environment (the human approval gate)

```bash
gh api -X PUT repos/djynnius/ferraus/environments/release \
  -F "reviewers[][type]=User" -F "reviewers[][id]=$(gh api user -q .id)" \
  -F "deployment_branch_policy[protected_branches]=false" \
  -F "deployment_branch_policy[custom_branch_policies]=true"
gh api -X POST repos/djynnius/ferraus/environments/release/deployment-branch-policies -f name='v*' -f type=tag
```

Also protect `main` (required CI checks) and add a tag protection rule for `v*`.

## 2. Registry signing keys (models and sounds)

```bash
# model registry
cargo run -q -p ferraus-models --example registry_sign -- keygen ~/model-registry.secret \
  > crates/ferraus-models/registry/registry.pub
gh secret set MODEL_REGISTRY_SIGNING_KEY --env release < ~/model-registry.secret
# sounds registry
cargo run -q -p ferraus-plugins --example sign_registry -- --generate > ~/sounds-registry.secret
sed -n 's/^public key  (commit): *//p' ~/sounds-registry.secret \
  > crates/ferraus-plugins/sounds/registry-public-key.txt
# re-sign the embedded registry so it verifies with the new public key
cargo run -q -p ferraus-plugins --example sign_registry -- crates/ferraus-plugins/sounds/registry.json \
  --key-file ~/sounds-registry.secret
sed -n 's/^private seed (keep secret): *//p' ~/sounds-registry.secret | tr -d '\n' \
  | gh secret set SOUNDS_REGISTRY_SIGNING_KEY --env release
git commit -am "Release registry public keys" && git push
```

Done for djynnius/ferraus on 2026-10-19; the private keys are kept outside the repository by
the maintainer. Repeat this section only to rotate the keys.

## 3. Homebrew tap

Create the public repository `djynnius/homebrew-tap` (empty is fine) and a fine-grained token
with contents read/write on it:

```bash
gh repo create djynnius/homebrew-tap --public --description "Homebrew formulae for ferraus"
gh secret set HOMEBREW_TAP_TOKEN --env release   # paste the token
```

## 4. macOS signing and notarization (off until you have a certificate)

Set `macos-sign = true` in `dist-workspace.toml` and run `dist generate`, then add the
secrets from an Apple Developer ID Application certificate exported as `.p12`:

```bash
base64 -i DeveloperID.p12 | gh secret set CODESIGN_CERTIFICATE --env release
gh secret set CODESIGN_CERTIFICATE_PASSWORD --env release
gh secret set CODESIGN_IDENTITY --env release        # e.g. "Developer ID Application: Name (TEAMID)"
```

## 5. Windows signing

cargo-dist signs Windows artifacts with SSL.com eSigner. Set `ssldotcom-windows-sign = "prod"` in
`dist-workspace.toml`, run `dist generate`, and add `SSLDOTCOM_USERNAME`, `SSLDOTCOM_PASSWORD`,
`SSLDOTCOM_CREDENTIAL_ID`, `SSLDOTCOM_TOTP_SECRET` to the `release` environment.

## 6. apt and dnf repositories (GitHub Pages)

```bash
gpg --quick-gen-key "ferraus release <release@ferraus.invalid>" rsa4096 sign never
gpg --armor --export-secret-keys "ferraus release" | gh secret set PACKAGE_SIGNING_KEY --env release
git switch --orphan gh-pages && git commit --allow-empty -m "Pages" && git push origin gh-pages && git switch main
gpg --armor --export "ferraus release" > docs/guides/ferraus-archive-keyring.asc   # publish the public key
```

Enable GitHub Pages from the `gh-pages` branch (Settings → Pages).

## 7. Package managers and crates.io

* crates.io: `cargo login`, then create an API token scoped to publish-new/publish-update and
  `gh secret set CARGO_REGISTRY_TOKEN --env release`. The first CI release reserves the names.
* winget: fork `microsoft/winget-pkgs`, create a classic token with `public_repo`, and
  `gh secret set WINGET_TOKEN --env release`.
* scoop: `gh repo create djynnius/scoop-bucket --public` and
  `gh secret set SCOOP_BUCKET_TOKEN --env release` with contents write on it.

## 8. Model hosting (recommended before 1.0)

Every verified model in `crates/ferraus-models/registry/registry.json` downloads from its
author's or exporter's repository (Hugging Face and GitHub releases), pinned by commit and
SHA-256, so a deleted upstream file breaks that feature. To remove that risk, upload the same
files as assets of a `djynnius/ferraus-models` release and put those URLs first in each entry's
`urls` (the hashes stay the same). The only model still missing is a license-clean lead/backing
vocal model for `separate --vocal-split`.

## 9. Legal review

Have counsel review `docs/legal/LICENSE-REVIEW.md` (VST3 host under GPLv3 as a separate
executable; Verovio under LGPL in the separate `ferraus-engrave` executable; model and sound
licenses) before the 1.0 release (PRD gates G3 and G4).
