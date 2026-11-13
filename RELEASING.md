# Releasing

Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`. It builds
Mightty.app for Apple Silicon and Intel, signs it with the Developer ID,
has Apple notarize it, and publishes a GitHub Release with `Mightty.dmg`,
the disk image people download, and `Mightty.zip`, the same app for scripts.
The download button on [mightty.org](https://mightty.org) points at
`https://github.com/mightty-org/mightty/releases/latest/download/Mightty.dmg`,
so it serves the new release as soon as it's published.

## Making a release

```sh
scripts/release.sh 0.2.0     # opens a pull request that sets the version
scripts/release.sh --tag     # once it's merged: tags main and pushes the tag
```

1. `scripts/release.sh 0.2.0` sets `version` in `Cargo.toml` and the crates'
   versions in `Cargo.lock` on a branch, and opens a pull request. The
   version is in the source because the app, its About panel and its update
   check are built from it.
2. Merge the pull request when CI has passed.
3. `scripts/release.sh --tag` tags the merged commit with that version and
   pushes the tag, which starts the release workflow. The workflow refuses a
   tag that differs from `Cargo.toml` or isn't on `main`.
4. Approve the run under Actions and follow it. Notarization usually takes
   a few minutes.
5. Add a section to the website's changelog.

A tag with a hyphen, such as `v0.2.0-beta.1`, makes a pre-release, which
the download link skips. Release tags can't be moved or deleted once
pushed (see `.github/rulesets/tags.json`); fix a bad release with a new
version.

## Who can release

A release is signed with the Developer ID, so three things guard it:

- Only organization admins can create a `v*` tag (`.github/rulesets/tags.json`).
- The workflow refuses a tag whose commit isn't on `main`, where every
  change has been through a pull request.
- The signing secrets belong to the `release` environment, not to the
  repository, so no other workflow can read them.

## Secrets, set up once

The `release` environment (Settings › Environments) holds the secrets and
takes deployments from `v*` tags only. Once the repository is public, add
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

## Building a release by hand

```sh
scripts/bundle.sh --universal --sign "Developer ID Application: NAME (TEAM)"
```

`rustup target add x86_64-apple-darwin` first. Notarize with
`xcrun notarytool submit` as the workflow does, then
`xcrun stapler staple target/release/Mightty.app`, and pack it with
`scripts/make-dmg.sh --sign "Developer ID Application: NAME (TEAM)"`.
