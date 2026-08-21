# Releasing MikMik

Three artefacts ship separately: the GitHub release (binaries), the npm package, and the VS Code extension. Only the first is automated today.

## Order

1. Cut the GitHub release, which stamps the version itself. The installers read its assets.
2. Publish to npm. The workflow refuses to run before the release exists.
3. Publish the VS Code extension. Independent of the other two.

## Version stamping

`--release` stamps the version as part of cutting a release, so the script below is only needed when stamping outside that flow. It fails loudly if an expected pattern is missing, which also makes it a cheap check that no surface drifted after a rename or a restructure: run it with the current version and it rewrites nothing.

```bash
python scripts/bump-version.py vX.Y.Z
```

Versioning is forward-only; the release workflow refuses a tag less than or equal to the highest existing tag. Never edit `src-rust/Cargo.lock` or the `version` field in `npm/package.json` by hand.

## 1. GitHub release

Triggered by a marker in the head commit message, handled by `.github/workflows/auto-release.yml`:

- `--release vX.Y.Z` cuts a new release. The tag must be strictly greater than the highest existing one. The workflow stamps the version itself, commits the bump as `github-actions[bot]` with `[skip ci]`, then dispatches `release.yml`. Running `scripts/bump-version.py` by hand first is therefore optional for this path.
- `--patch` patches the currently shipped release in place, reading the version from `src-rust/Cargo.toml`. Restricted to the `KilimcininKorOglu` actor, because a patch force-moves a published tag.

`release.yml` builds five targets and publishes archives named `mikmik-<os>-<arch>`. `install.sh` and `install.ps1` read exactly these names, so a mismatch breaks the one-line installer rather than failing loudly.

Nothing else is needed: the workflow runs under `GITHUB_TOKEN`, and it hands off to `npm-publish.yml` explicitly rather than relying on the `workflow_run` trigger.

## 2. npm

Package name: `mikmik`. Wrapper lives in `npm/`; `install.js` downloads the prebuilt binary for the platform on postinstall.

### State as measured

- `mikmik` is unclaimed on the registry (`registry.npmjs.org/mikmik` returns 404).
- The `claurst` package on npm belongs to **`kuberwastaken`**, the upstream author. This fork has no rights over it. Do not plan to deprecate or redirect it.
- The npm account `kilimcininkoroglu` exists and already maintains one package.

### First publish (manual, once)

`npm-publish.yml` authenticates through OIDC with no token (`NODE_AUTH_TOKEN` and `NPM_TOKEN` are both empty). That requires a trusted publisher, which cannot be attached to a package that does not exist yet. So the first version is published from a machine:

```bash
cd npm
npm login                 # account: kilimcininkoroglu
npm publish --provenance --access public
```

npm requires 2FA for creating and publishing a package. Either complete the OTP prompt interactively, or use a granular access token with "bypass 2FA" enabled.

`--access public` is required because the name is unscoped and new. `--provenance` matches what CI publishes afterwards.

### Then wire CI (once)

After the package exists, register the workflow as a trusted publisher so every later release publishes itself:

```bash
npm trust github mikmik \
  --file npm-publish.yml \
  --repo KilimcininKorOglu/mikmik \
  --allow-publish
```

The equivalent settings in the npm web UI, which `npm-publish.yml` prints at run time:

| Field | Value |
|---|---|
| Organization or user | `KilimcininKorOglu` |
| Repository | `mikmik` |
| Workflow filename | `npm-publish.yml` |
| Environment name | *(empty)* |

The workflow verifies that `npm/package.json`'s `repository.url` equals `git+https://github.com/KilimcininKorOglu/mikmik.git` and fails the run if it does not, so the repository rename and the trusted-publisher record have to stay in step.

### Later releases

Automatic. `release.yml` dispatches `npm-publish.yml`, which resolves the version from `src-rust/Cargo.toml`, verifies the GitHub release exists, and publishes. It can also be run by hand from the Actions tab with a version input.

## 3. VS Code extension

Extension id: `kilimcininkoroglu.mikmik-vscode`. Source in `editors/vscode/`.

### State as measured

- The publisher `kilimcininkoroglu` does **not** exist on the Marketplace (`marketplace.visualstudio.com/publishers/kilimcininkoroglu` returns 404). It has to be created before anything can be published.
- Nothing is published under that publisher, so there is no old extension id to deprecate.
- The publisher does not exist on Open VSX either. Publishing there is optional and separate.
- There is no CI workflow for the extension; publishing is manual.

`src-rust/crates/core/src/ide.rs` prints `code --install-extension kilimcininkoroglu.mikmik-vscode` to users, so the published id has to match that string exactly.

### Create the publisher (once)

1. Sign in at <https://marketplace.visualstudio.com/manage> with the Microsoft account that owns the extension.
2. Create a publisher with the id `kilimcininkoroglu`. The id is permanent and cannot be renamed; the display name can change.

### Get a Personal Access Token (once)

From Azure DevOps (<https://dev.azure.com>), under user settings, Personal Access Tokens:

- Organization: **All accessible organizations**. A token scoped to a single organization is rejected.
- Scope: **Marketplace → Manage**.

Then verify it:

```bash
cd editors/vscode
npx vsce login kilimcininkoroglu
```

### Publish

```bash
cd editors/vscode
npm ci
npm run check          # tsc over the extension, the webview and the tests
npm test
npx vsce publish --no-dependencies
```

`vscode:prepublish` runs the type check and a production esbuild, so `vsce publish` builds what it ships. `--no-dependencies` matches the existing `npm run package` script: the extension bundles through esbuild and must not ship `node_modules`.

To inspect the artefact before publishing:

```bash
npm run package        # writes mikmik-vscode-X.Y.Z.vsix
```

### Optional: publish from CI without a token

`vsce` supports OIDC, which removes the stored PAT:

```yaml
permissions:
  contents: read
  id-token: write
steps:
  - run: npx @vscode/vsce publish --oidc
```

This needs a trusted publishing policy configured on the Marketplace for the publisher. Worth doing only once the extension ships regularly.

## What is not automated

- The first npm publish, because trusted publishing needs an existing package.
- Everything about the VS Code extension.
- Open VSX, which is not set up at all.
