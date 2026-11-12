# Releasing

Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`. It builds
Mightty.app for Apple Silicon and Intel, signs it with the Developer ID,
has Apple notarize it, and publishes a GitHub Release with `Mightty.zip`.
The website's download button points at
`https://github.com/mightty-org/mightty/releases/latest/download/Mightty.zip`,
so it serves the new release as soon as it's published.

## Making a release

1. Set `version` in `Cargo.toml` (the workspace's) and commit it through a
   pull request. The release fails if the tag and the version differ.
2. Tag the merged commit on `main` and push the tag:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. Follow the run under Actions. Notarization usually takes a few minutes.
4. Add a section to the website's changelog.

A tag with a hyphen, such as `v0.2.0-beta.1`, makes a pre-release, which
the download link skips. Release tags can't be moved or deleted once
pushed (see `.github/rulesets/tags.json`); fix a bad release with a new
version.

## Secrets, set up once

Under Settings › Secrets and variables › Actions in the repository:

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
gh secret set MACOS_CERTIFICATE --repo mightty-org/mightty < <(base64 -i DeveloperID.p12)
gh secret set MACOS_CERTIFICATE_PASSWORD --repo mightty-org/mightty
rm DeveloperID.p12
```

**The notary key.** In App Store Connect, Users and Access › Integrations ›
App Store Connect API › Team Keys, generate a key with the Developer role.
Download the `.p8` (it can only be downloaded once), and note its key ID
and the issuer ID. Then:

```sh
gh secret set NOTARY_KEY --repo mightty-org/mightty < AuthKey_XXXXXXXXXX.p8
gh secret set NOTARY_KEY_ID --repo mightty-org/mightty
gh secret set NOTARY_ISSUER_ID --repo mightty-org/mightty
```

Keep the `.p8` somewhere safe, or revoke the key once the secret is set and
make a new one when it's needed.

## Building a release by hand

```sh
scripts/bundle.sh --universal --sign "Developer ID Application: NAME (TEAM)"
```

`rustup target add x86_64-apple-darwin` first. Notarize with
`xcrun notarytool submit` as the workflow does, then
`xcrun stapler staple target/release/Mightty.app`.
