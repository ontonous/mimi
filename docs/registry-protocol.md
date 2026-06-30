# Mimi Registry Protocol (Draft)

> **Status**: Draft — v0.28.12. Subject to change before v1.0.
> **Compatibility**: Designed for the v0.28.x package manager. Earlier
> versions of `mimi` do not speak this protocol; they only resolve
> dependencies from a local registry directory at `~/.mimi/registry/`.

This document specifies the minimum JSON API that a Mimi package
registry must implement. The protocol is intentionally tiny: it does
not try to mirror npm/PyPI/Cargo; it is just enough to support
`mimi add`, `mimi install`, `mimi tree`, and `mimi update`.

---

## 1. Endpoints

All endpoints are HTTPS, JSON in/out, and live under the registry
base URL. The default base URL is `https://registry.mimi-lang.org`.
A project can override it by setting `[registry].url` in
`mimi.toml`.

### 1.1 `GET /v1/packages/{name}`

Returns metadata for a single package, including all available
versions.

**Response 200**

```json
{
  "name": "mimi-http",
  "description": "Tiny HTTP client for Mimi.",
  "homepage": "https://github.com/mimilang/mimi-http",
  "license": "Apache-2.0",
  "versions": [
    "0.1.0",
    "0.1.1",
    "0.2.0"
  ]
}
```

**Response 404**

```json
{ "error": "package not found", "name": "mimi-http" }
```

### 1.2 `GET /v1/packages/{name}/{version}`

Returns the manifest of a specific released version. This is the
`mimi.toml` of the published package verbatim, plus a checksum.

**Response 200**

```json
{
  "manifest": "[package]\nname = \"mimi-http\"\nversion = \"0.2.0\"\n...\n",
  "tarball_url": "https://registry.mimi-lang.org/v1/tarballs/mimi-http/0.2.0.tar.gz",
  "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
  "size": 4821,
  "dependencies": [
    { "name": "mimi-bytes", "version": "^0.1" }
  ]
}
```

**Response 404**

```json
{ "error": "version not found", "name": "mimi-http", "version": "0.2.0" }
```

### 1.3 `GET /v1/tarballs/{name}/{version}.tar.gz`

Returns the source tarball. The tarball's root must be the package
directory itself (so `tar -xzf` produces a single folder containing
`mimi.toml` and source files). The tarball layout MUST match the
`mimi publish` output.

**Response 200** — `Content-Type: application/gzip`, body is the
gzip-compressed tar archive.

### 1.4 `GET /v1/search?q={query}`

Optional. Used by `mimi search <query>`. Returns up to 50 results
ranked by name prefix match.

**Response 200**

```json
{
  "results": [
    { "name": "mimi-http", "description": "Tiny HTTP client.", "latest": "0.2.0" },
    { "name": "mimi-https", "description": "HTTPS variant.", "latest": "0.1.0" }
  ]
}
```

---

## 2. Version Constraints

`mimi` reuses the [semver](https://semver.org/) version requirement
syntax. The following forms are accepted in `mimi.toml`:

| Form | Meaning | Example |
|------|---------|---------|
| `*` | Any version | `version = "*"` |
| `1.2.3` | Exact match (no semver operators) | `version = "1.2.3"` |
| `=1.2.3` | Exact match | `version = "=1.2.3"` |
| `^1.2` | Compatible release (same major, ≥ minor) | `version = "^1.2"` |
| `~1.2` | Patch-level updates only | `version = "~1.2"` |
| `>=1.0, <2.0` | Comma-separated range | `version = ">=1.0, <2.0"` |

When multiple versions are available, `mimi` picks the **highest**
matching version (lexicographic, semver-aware).

---

## 3. Dependency Sources

A dependency in `mimi.toml` can come from one of three sources,
in priority order:

1. **path**: a local filesystem path. Use while developing libraries
   side-by-side. The path is copied to `.mimi/deps/<name>` on
   `mimi install`.
   ```toml
   [[dependencies]]
   name = "mimi-utils"
   path = "../mimi-utils"
   ```
2. **git**: a Git repository URL. Optional `tag` (default: `main`).
   ```toml
   [[dependencies]]
   name = "experimental"
   git = "https://github.com/mimilang/experimental"
   tag = "v0.1"
   ```
3. **registry** (default): a name and optional version constraint.
   ```toml
   [[dependencies]]
   name = "mimi-http"
   version = "^0.2"
   ```

A dep may not have more than one source; the resolver rejects
ambiguous specifications.

---

## 4. Lockfile Format

`mimi.lock` is a TOML file at the project root. It pins every
transitively-resolved package to a specific version, source, and
content checksum.

```toml
[[package]]
name = "mimi-http"
version = "0.2.0"
source = "registry"                # or "git+https://..." or "path:/abs/path"
checksum = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"

[[package]]
name = "mimi-bytes"
version = "0.1.3"
source = "registry"
checksum = "abc123..."
```

The checksum is the FNV-1a 64-bit hex digest of all files in the
package directory, sorted by relative path. This is purely a
corruption check; the registry is trusted to serve the version it
claims.

---

## 5. Local Cache

By default `mimi` uses `~/.mimi/registry/` as a *local* registry
mirror. When a package is published with `mimi publish`, the
resulting `<name>/<version>/` directory is placed here. The
`install` command resolves from this directory first, falling back
to the network when a name is missing.

This dual model (local mirror + remote registry) is what allows
`mimi` to work fully offline once dependencies have been fetched
once: pass `--offline` to refuse any network operation.

---

## 6. Authentication (Future)

Authentication is **not** part of v0.28.12. The protocol assumes
public-read, authenticated-publish. Once publishing is implemented,
`mimi publish` will use a Bearer token from `~/.mimi/credentials`.

The reserved `Authorization` header is the only auth mechanism that
will be standardized.

---

## 7. Error Codes

| HTTP | Meaning | Client behavior |
|------|---------|-----------------|
| 200 | OK | proceed |
| 301 | Permanent redirect (registry moved) | update base URL, retry once |
| 400 | Malformed request | fail loudly, do not retry |
| 404 | Package or version not found | try next candidate; if all fail, error |
| 410 | Version yanked | pick a different version |
| 429 | Rate-limited | backoff with jitter, max 3 retries |
| 500–599 | Server error | backoff, max 3 retries, then fail |

---

## 8. Versioning of the Protocol Itself

The `/v1/` URL prefix is mandatory and frozen for the v1.x
registry-protocol lifetime. Breaking changes will require a
`/v2/` prefix; v0.28.x clients will not be able to talk to a v2
server without an explicit version-bump of `mimi`.

---

*End of draft — v0.28.12.*
