# Releasing

Releases are made by the Release workflow
(`.github/workflows/release.yml`), started by hand.

## Making a release

1. Open **Actions › Release › Run workflow** on GitHub, on `main`, and
   choose which part of the version to raise: `patch` (0.2.0 → 0.2.1),
   `minor` (0.2.0 → 0.3.0) or `major` (0.2.0 → 1.0.0). From a terminal:
   `gh workflow run release.yml -f bump=minor`.
2. Approve the run when GitHub asks; it waits for a reviewer of the
   `release` environment.
3. Add a section to the website's changelog.

The run sets the new version in `Cargo.toml` and `Cargo.lock`, runs the
tests, builds Mightty.app for Apple Silicon and Intel, signs it with the
Developer ID and has Apple notarize it. Only then does it push the
version's commit and its tag `vX.Y.Z` to `main`, and publish a GitHub
Release with `Mightty.dmg`, the disk image people download, `Mightty.zip`,
the same app for scripts and for the app's own updater, and `SHA256SUMS`.
A run that fails before that leaves no commit, tag or release behind.

The release's notes are the ones GitHub generates: the pull requests merged
since the last release, grouped by their labels as `.github/release.yml`
says. Label a pull request `enhancement`, `bug`, `security` or
`performance` to put it under that heading.

The download button on [mightty.org](https://mightty.org) points at
`https://github.com/mightty-org/mightty/releases/latest/download/Mightty.dmg`,
so it serves the new release as soon as it's published, and installed
copies of Mightty find it with their update check. Release tags can't be
moved or deleted once pushed (see `.github/rulesets/tags.json`); fix a bad
release with a new version.

## Who can release

A release is signed with the Developer ID, so these guard it:

- The workflow only runs on `main`, where every change has been through a
  pull request.
- The signing secrets belong to the `release` environment, not to the
  repository, so no other workflow can read them, and the environment
  asks a reviewer to approve each run.
- `main` takes changes by pull request only, and `v*` tags are for
  organization admins to make. The one exception is the release's deploy
  key, which the rulesets let past exactly those two rules, so the workflow
  can push the version's commit and create its tag. It can't delete `main`
  or rewrite its history, nor move or delete a tag: those rules are in
  rulesets of their own that only admins get past. The key is a secret of
  the `release` environment and is used in a single step, after the build
  has passed. The repository should have no other deploy key with write
  access.

## Secrets, set up once

The `release` environment (Settings › Environments) holds the secrets and
takes deployments from `main` only. Once the repository is public, add
yourself there as a required reviewer, so each release waits for your
approval; GitHub doesn't offer that for a private repository on the free
plan, nor the rulesets. Add these as secrets of the environment:

| Secret | What it is |
|---|---|
| `MACOS_CERTIFICATE` | The Developer ID Application certificate and its private key, as a `.p12`, base64-encoded |
| `MACOS_CERTIFICATE_PASSWORD` | The password the `.p12` was exported with |
| `NOTARY_KEY` | An App Store Connect API key: the text of the `.p8` file |
| `NOTARY_KEY_ID` | That key's ID |
| `NOTARY_ISSUER_ID` | The issuer ID shown above the keys |
| `RELEASE_DEPLOY_KEY` | The private half of a deploy key with write access, which the rulesets let push to `main` and make `v*` tags |

**The certificate.** In Keychain Access, under My Certificates, find
"Developer ID Application: …", open it to check it has its private key,
then File › Export Items… as a `.p12` with a strong password. Then:

```sh
gh secret set MACOS_CERTIFICATE --env release --repo mightty-org/mightty < <(base64 -i DeveloperID.p12)
gh secret set MACOS_CERTIFICATE_PASSWORD --env release --repo mightty-org/mightty
rm DeveloperID.p12
```

**The notary key.** In App Store Connect, Users and Access › Integrations ›
App Store Connect API › Team Keys, generate a key with the Developer role.
Download the `.p8` (it can only be downloaded once), and note its key ID
and the issuer ID. Then:

```sh
gh secret set NOTARY_KEY --env release --repo mightty-org/mightty < AuthKey_XXXXXXXXXX.p8
gh secret set NOTARY_KEY_ID --env release --repo mightty-org/mightty
gh secret set NOTARY_ISSUER_ID --env release --repo mightty-org/mightty
```

Keep the `.p8` somewhere safe, or revoke the key once the secret is set and
make a new one when it's needed.

**The deploy key.** Make a key pair, add the public half to the repository
as a deploy key with write access, and store the private half as the
secret:

```sh
ssh-keygen -t ed25519 -N "" -C "mightty release" -f release-key
gh repo deploy-key add release-key.pub --repo mightty-org/mightty --allow-write --title "Release workflow"
gh secret set RELEASE_DEPLOY_KEY --env release --repo mightty-org/mightty < release-key
rm release-key release-key.pub
```

`scripts/apply-rulesets.sh` applies the rulesets in `.github/rulesets`,
which give deploy keys their way past those two rules.

## Building a release by hand

```sh
scripts/bundle.sh --universal --sign "Developer ID Application: NAME (TEAM)"
```

`rustup target add x86_64-apple-darwin` first. Notarize with
`xcrun notarytool submit` as the workflow does, then
`xcrun stapler staple target/release/Mightty.app`, and pack it with
`scripts/make-dmg.sh --sign "Developer ID Application: NAME (TEAM)"`.
