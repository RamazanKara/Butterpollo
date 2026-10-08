# HTTP API

[Docs](README.md) · [Getting started](getting-started.md) · [Configuration](configuration.md)

Butterpollo's Windows Rust host serves its administration API at `https://localhost:47990/api`. The console uses the same API. Changing the base `port` setting moves the console to `port + 1`.

This guide covers the current Rust implementation in [web.rs](../rust/host/src/web.rs). Moonlight pairing, app discovery and streaming use separate protocol endpoints in [nvhttp.rs](../rust/host/src/nvhttp.rs). The [historical C++ API guide](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.23/docs/legacy/api-cpp.md) is kept at the 2.0.0-rc.23 tag as a migration reference.

## Authentication

Create the administrator account through the local console first. Initial account creation is restricted to requests from the host PC. Access to the web interface also follows `origin_web_ui_allowed`: `pc`, `lan` (the default), or `wan`.

| Method | Use |
| --- | --- |
| HTTP Basic | Send the administrator username and password over HTTPS. Useful for interactive scripts. |
| Scoped API token | Send `Authorization: Bearer <token>`. Each token grants only the selected path and HTTP methods. |
| Web session | Sign in with `POST /api/auth/login`. The host issues secure, HttpOnly session and refresh cookies; it also returns the access, refresh and CSRF tokens in JSON. |

The public API routes are `/api/auth/status`, `/api/auth/login`, `/api/auth/refresh`, `/api/csrf-token` and `/api/configLocale`. Other API routes require authentication, apart from local first-time account creation.

### Scoped tokens

Use the console's API token controls, or these administrator-authenticated routes:

| Method and path | Result |
| --- | --- |
| `GET /api/token/routes` | Available scope paths and their allowed methods. |
| `POST /api/token` | Create a token from a `scopes` array; returns its secret once. |
| `GET /api/tokens` | Token hashes, ownership, creation times and scopes. |
| `DELETE /api/token/{hash}` | Revoke a token. |

A read-only token request body can be:

```json
{
  "scopes": [
    { "path": "/api/metadata", "methods": ["GET"] },
    { "path": "/api/session/status", "methods": ["GET"] }
  ]
}
```

Choose paths exactly as returned by `/api/token/routes`, including any pattern entries. The host validates the selected methods and matches each scope against the whole request path. Token-management routes cannot themselves be granted as token scopes. Secrets are stored as hashes; keep the creation response if the token will be used later.

### Sessions and CSRF

Browser sign-in takes `username`, `password` and optional `remember_me`. `POST /api/auth/refresh` rotates the refresh token; `POST /api/auth/logout` revokes the current session. `GET /api/auth/sessions` lists sessions, and `DELETE /api/auth/sessions/{id}` revokes one. Remembered sessions survive host restarts.

For requests that change state:

- If an `Origin` header is present, it must match the host's HTTPS origin or an entry in `csrf_allowed_origins`. A CSRF token does not override a rejected origin.
- Session authentication, whether through a cookie or a Bearer access token, requires `X-CSRF-Token` on protected routes. Read the session's token from `GET /api/csrf-token` using the same session.
- Basic authentication and valid scoped API tokens do not require a session CSRF token. Origin checks still apply.
- Before browser sign-in or first-time account creation, fetch `/api/csrf-token`, retain its anonymous cookie, and send the returned token as `X-CSRF-Token`. If that cookie is present, the host requires a matching header.

## Read-only examples

These PowerShell examples read host status without launching an app or changing settings. Replace the certificate path with the public certificate from the active profile and `admin` with your administrator name. `curl.exe --user admin` prompts for the password instead of putting it in the command.

A newly created Rust profile includes `localhost` in its certificate. For an imported or custom certificate, use its matching HTTPS name and trusted certificate instead.

```powershell
$certificate = 'C:\path\to\profile\credentials\cacert.pem'
curl.exe --fail-with-body --silent --show-error --cacert $certificate --user admin https://localhost:47990/api/metadata
curl.exe --fail-with-body --silent --show-error --cacert $certificate --user admin https://localhost:47990/api/session/status
curl.exe --fail-with-body --silent --show-error --cacert $certificate --user admin https://localhost:47990/api/token/routes
```

`/api/metadata` (also served as `/api/meta`) reports the host version, codec probe status, configured capture backend, displays, audio endpoints and virtual-display capabilities. Results are cached for up to five seconds; reading them does not start a codec probe. Codec state is `checking`, `ready` or `failed`.

`/api/session/status` includes `activeSessions`, `appRunning`, `appName`, `paused` and `running`. An app can remain running after its stream disconnects, so these fields describe different states.

## Endpoint groups

The routes below are implemented by the Rust host. JSON requests use `Content-Type: application/json`; most responses include a Boolean `status`. Display lists, images and log downloads have their own response formats.

| Area | Read | Change |
| --- | --- | --- |
| Host settings | `GET /api/config`, `GET /api/metadata`, `GET /api/configLocale` | `POST` or `PATCH /api/config` |
| Applications | `GET /api/apps`, `GET /api/apps/{id}/cover`, `GET /api/apps/{uuid}/icon` | `POST /api/apps`, `DELETE /api/apps/{id}` (or `POST /api/apps/delete` with `{"uuid": ...}`), `POST /api/apps/reorder`, `POST /api/apps/launch`, `POST /api/apps/close` |
| Paired devices | `GET /api/clients/list`, `GET /api/clients/pending` | `POST /api/pin`, `POST /api/otp`, `POST /api/clients/update`, `POST /api/clients/disconnect`, `POST /api/clients/unpair`, `POST /api/clients/unpair-all` |
| Active sessions | `GET /api/session/status`, `GET /api/rtsp/sessions` | Use the application or device controls above. |
| Displays | `GET /api/display-devices`, `GET /api/clients/display-layout`, `GET /api/clients/hdr-profiles`, `GET /api/display/golden_status` | `PUT /api/clients/display-layout`, `POST /api/display/export_golden`, `POST /api/display/restore_golden`, `DELETE /api/display/golden`, `POST /api/display/terminate_virtual`, `POST /api/reset-display-device-persistence` |
| Frame limiting and HDR | `GET /api/frame-limiter/status`, `GET /api/rtss/status`, `GET /api/health/vulkan-hdr-layer`, `GET /api/framegen/edid-refresh?device_id=...` | `POST /api/health/vulkan-hdr-layer/register`, `POST /api/apps/rtx_hdr/live` |
| Steam | `GET /api/steam/status`, `GET /api/steam/games` | `POST /api/steam/force_sync`, `POST /api/steam/launch` |
| Playnite | `GET /api/playnite/status`, `GET /api/playnite/games`, `GET /api/playnite/categories` | `POST /api/playnite/install`, `POST /api/playnite/uninstall`, `POST /api/playnite/force_sync`, `POST /api/playnite/cover`, `POST /api/playnite/launch` |
| Library extras | `GET /api/covers/{id}`, `GET /api/lossless_scaling/status`, `GET /api/browse?path=...&type=...` | `POST /api/covers/upload`, `POST /api/apps/purge_autosync` |
| Updates | `GET /api/updates` | `POST /api/updates/check`, `POST /api/updates/install`, `POST /api/updates/cancel` |
| Diagnostics | `GET /api/logs`, `GET /api/logs/tail`, `GET /api/logs/export`, `GET /api/logs/export_crash`, `GET /api/logs/export_crash/manifest`, `GET /api/health/crashdump` | `POST /api/health/crashdump/dismiss` |
| Administration | Authentication and token routes above. | `POST /api/password`, `POST /api/restart`, `POST /api/quit` |

Useful response and request details:

- Configuration writes merge submitted fields and return `restart_required: true`. Treat the response as a saved configuration, not confirmation that every active stream has adopted it.
- Applications are updated by their `uuid`; deletion accepts the UUID or numeric app ID. Read `/api/apps` before making changes rather than treating an array index as the identity.
- `/api/logs` and `/api/logs/export` return up to the last 8 MiB of the current log as text. `/api/logs/export_crash` creates the support ZIP. The ordinary log export is not a ZIP.
- `/api/apps/{uuid}/icon` returns the PNG icon a Playnite sync saved for the app, or `404`.
- `/api/browse` lists a folder of this PC for a file picker: `path`, `parent` and `entries` (each with `name`, `path` and `type` `directory` or `file`), folders first. `type=executable` lists only `.exe`, `.bat`, `.cmd` and `.ps1` files, `type=file` any file, `type=directory` no files; folders are always listed. A file or missing path lists the nearest existing folder above it; an empty `path` lists the drives, and a drive root's `parent` is empty.
- `/api/playnite/cover` takes `playnite_id` and `cover_key` (an image saved by `/api/covers/upload` or a cover search), asks the Playnite plugin to use that image as the game's cover, then syncs the library and returns the cover's `path`. `/api/playnite/launch` restarts Playnite in desktop mode.
- `/api/logs/tail` accepts `offset` and `max`, then returns `text`, the next `offset`, `size` and a `reset` flag for log rotation.
- Update installation is queued for an idle installed service. Read `/api/updates` for progress and the result.

Authentication failures return HTTP `401`; denied origins or network reach return `403`; invalid requests commonly return `400` with `{"status":false,"error":"..."}`. Login rate limiting returns `429`. Check the HTTP status and response body together.

For exact request fields beyond this overview, follow [the API handler](../rust/host/src/web.rs), [token scope validation](../rust/core/src/auth.rs) and [administration checks](../rust/tests/web_api.py).
