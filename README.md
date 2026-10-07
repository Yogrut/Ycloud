# Ycloud

English | [简体中文](README.zh-CN.md)

A self-hosted file manager built with Rust and Vue, with local storage and S3-compatible object storage.

## Features

- Local storage, Alibaba Cloud OSS, Tencent Cloud COS, MinIO, RustFS, and other S3-compatible services.
- Uploads, downloads, previews, search, move, copy, rename, delete, ZIP archives, and an image gallery.
- Separate access rules for administrators, guests, user accounts, and WebDAV mounts.
- Password-protected folders, TOTP, login protection, access logs, and transfer limits.
- Direct S3 uploads from the browser, streaming relay uploads, atomic local writes, and automatic recovery of interrupted uploads.

Browser uploads never overwrite a file with the same name. Ycloud saves the new file with a numbered suffix and shows its saved name in the upload task. WebDAV keeps the client's normal write and overwrite behavior. Administrators create user accounts; the user icon opens user sign-in, and the shield opens administrator sign-in. Only one administrator session can be active at a time.

Successful sign-out revokes the current browser's account session, site access token, and folder access tokens. An unconfirmed sign-out does not redirect or report success.

See [Backend design and maintenance](docs/backend-design.md) for commit boundaries, recovery rules, and implementation details.

## Screenshots

<table>
  <tr>
    <th>Site access</th>
    <th>File browser</th>
  </tr>
  <tr>
    <td><img src="docs/images/web-login.png" alt="Ycloud site access"></td>
    <td><img src="docs/images/file-browser.png" alt="Ycloud file browser"></td>
  </tr>
  <tr>
    <th>User sign-in</th>
    <th>Administrator sign-in</th>
  </tr>
  <tr>
    <td><img src="docs/images/user-login.png" alt="Ycloud user sign-in"></td>
    <td><img src="docs/images/admin-login.png" alt="Ycloud administrator sign-in"></td>
  </tr>
</table>

Admin dashboard:

![Ycloud admin dashboard](docs/images/admin-dashboard.png)

## Development and builds

- Rust and Cargo: `rust-toolchain.toml` pins Rust 1.96.1. Install the native build tools for your platform.
- Node.js: `^20.19.0` or `>=22.12.0`; npm: 11.17.0. Frontend dependencies are pinned in `frontend/package-lock.json`.
- Container deployments also need Docker Engine and the Compose plugin.

From the repository root, build the frontend before the backend. Rust embeds the files in `static/app/` into the executable:

```bash
cd frontend
npm ci
npm run build
cd ..
cargo build --release --locked
```

The executable is `target/release/ycloud`, or `ycloud.exe` on Windows. After changing the frontend, rebuild and commit the entire `static/app/` directory, including its chunks and generated `embedded-assets.rs`. Copying only the entry-point JavaScript is not enough.

For development, run `cargo run --locked` from the repository root and `npm run dev` from `frontend/` in another terminal. Open `http://127.0.0.1:5173`. Vite forwards API requests to `127.0.0.1:18473` by default. Initial credentials are written to `initial-credentials.json` in the working directory; do not commit that file.

## Docker Compose

From the repository root:

```bash
mkdir -p data storage
sudo chmod 700 data
docker compose up -d --build
docker compose ps
```

The first start creates accounts and configuration, but does not add storage. Sign in to the admin console and add local or S3 storage. Removing a storage configuration does not delete its files. The file browser lists storage according to permissions and remembers each account's last manually selected storage.

Open `http://<server-ip>:18473`. Read the initial credentials:

```bash
sudo cat ./data/initial-credentials.json
```

The credentials file is removed after you change both the administrator password and the site access password. With separate mounts, `./data` holds configuration, the master key, and runtime state; `./storage` holds local files. Back up both. With only a `./data` mount, local files use its `storage/` subdirectory by default.

The container runs as root by default; it does not enforce a UID or GID. To use another account, set `user: "UID:GID"` in Compose and give that account read and write access to the data directory. For example, a new, empty directory can use `sudo chown 1000:1000 data` with `user: "1000:1000"`. Back up existing data and check ownership and NAS ACLs before changing permissions recursively.

You can set `stop_grace_period: 60s` in Compose, or allow longer, such as `5m`, for large transfers. Stop the service after transfers finish where possible. Without an explicit setting, Docker waits 10 seconds before forcing termination. A longer wait does not guarantee that every task will finish.

Container stdout logs follow the Docker daemon's logging configuration unless overridden in Compose. This does not limit application access logs in the data directory. Recreate existing containers to apply new logging settings.

### Docker without Compose

Build and start from the repository root:

```bash
docker build --tag ycloud:local .
mkdir -p data
sudo chmod 700 data
docker run -d --name ycloud --restart unless-stopped \
  -p 18473:18473 \
  --mount type=bind,src="$(pwd)/data",dst=/var/lib/ycloud \
  ycloud:local
```

Add `--user UID:GID` or `--stop-timeout 60` before the image name to choose the runtime account or shutdown wait. The data directory and initial credentials work the same way as with Compose.

## Run directly on Linux

After building, run from the repository root:

```bash
BIND_ADDRESS=0.0.0.0 ./target/release/ycloud
```

By default, configuration is stored in `config.json` and local storage uses `storage/` in the working directory.

## Deployment settings

| Variable | Purpose |
| --- | --- |
| `BIND_ADDRESS` | Listening address; defaults to `127.0.0.1` outside the container |
| `PORT` | HTTP port; defaults to `18473` |
| `CONFIG_PATH` | Configuration file path |
| `STORAGE_PATH` | Local path available for administrators to add as storage |
| `ALLOWED_HOSTS` | Allowed internal Host values, separated by commas |
| `LOCAL_STORAGE_MOUNTS` | JSON array of local mount points available in the admin console |
| `RUST_LOG` | Log level |

The Dockerfile sets the container's listening address, port, and data paths. A normal Compose deployment does not need additional access-mode settings.

To record client IPs behind a reverse proxy, enter its source IP under **Trusted proxy IP** in the admin console's domain binding settings. The change is saved and takes effect immediately; no environment variable or restart is needed. Use the IP that Ycloud sees as the proxy's connection source, without a scheme or port. Separate multiple IPv4 or IPv6 addresses with commas. An empty list disables trust in forwarded headers. The proxy must overwrite `X-Real-IP`; Nginx Proxy Manager supplies this header by default. Existing logs are not rewritten. Keep the backend port accessible only to the proxy where possible.

### Configuration master key

Ycloud generates a master key and stores it in the data directory. Container deployments or external key management can use `YCLOUD_CONFIG_KEY_FILE`; `YCLOUD_CONFIG_KEY` can override it. The key is 32 random bytes encoded as URL-safe Base64. Losing it makes encrypted S3 SecretKeys and TOTP secrets unrecoverable.

### Storage

Changing a connection, restricting access, or disabling storage may interrupt active transfers. The backend handles unfinished uploads automatically. Do not repeat an upload while its result is awaiting confirmation. Removing storage does not delete files, but you must first remove references from WebDAV mounts, folder locks, and account permissions.

Guest access controls browser access without an account. WebDAV uses its own mount credentials and read/write permissions.

WebDAV shares the single-file upload limit, site-wide traffic quota, and global upload/download rate limits; `0` means no rate limit. Local storage accepts streaming uploads of unknown length. Downloads support a single Range; resumable uploads are not supported. Successful password checks use a small, short-lived in-memory cache, but every request still checks the current mount and permissions. A timeout does not prove that a file was not committed. Check the destination before sending it again.

Changing a global rate limit also affects data released by existing transfers. Removing the limit wakes waiting transfers; it cannot recall bytes already released. An unlimited rate does not bypass traffic quotas.

File downloads and WebDAV reads return attachment responses with content-isolation headers, without changing the file bytes. Opening a WebDAV file URL in a browser downloads it. Previews use a separate content-type policy; S3 Content-Type metadata does not decide whether content can be displayed inline.

HEAD returns metadata without reading the body. `If-Range` returns a partial response only when it matches the current S3 strong ETag. A different or unverifiable version returns the complete file. Local storage does not treat file size and modification time as a strong version: ordinary Range requests work, but requests with `If-Range` return the complete file.

WebDAV directory queries support `Depth: 0`, `Depth: 1`, selected properties, property names, and allprop/include. Recursive directory queries, including requests without a Depth header, return 403 with `propfind-finite-depth` rather than claiming a one-level listing is recursive. Clients should use Depth 0 or 1. Unsupported properties get a separate 404 property status. WebDAV locks and resumable uploads are not supported.

WebDAV PUT supports write preconditions. Failed preconditions return 412 without changing the destination. Local storage checks existence and modification time but does not invent a strong ETag. S3 uses version-conditional publication after checking the provider's atomic-condition support; an uncertain result is not retried or rolled back automatically. Write conditions on other methods, DAV If lock conditions, conditional OSS uploads, and conditional S3 uploads above 4 GiB are explicitly rejected. Conditions are never silently ignored. Unconditional uploads keep their existing behavior. See [Conditional WebDAV writes](docs/backend-design.md#conditional-webdav-writes).

Example of an additional local mount:

```env
LOCAL_STORAGE_MOUNTS=[{"id":"archive","name":"Archive","path":"/srv/ycloud/archive"}]
```

For self-hosted S3 services such as MinIO or RustFS, enter the Endpoint, Bucket, and access keys in the admin console. No endpoint allowlist is required. Endpoints must be HTTP(S) addresses without embedded credentials, a path, or a query string. HTTP is available on trusted internal networks; use HTTPS on untrusted networks.

Direct uploads require the browser to reach the configured Endpoint. The bucket's CORS policy must allow `PUT` from the Ycloud site's origin, with request headers `*`. An HTTPS site needs an HTTPS storage endpoint; browsers block HTTP mixed content. Access keys stay on the server. Ycloud issues short-lived upload URLs tied to temporary objects and parts, then verifies the parts before publishing the final file.

Enable relay uploads when the server can reach storage but the browser cannot. This is not limited to internal storage. Upgrades retain relay mode for existing S3 configurations; new configurations default to direct uploads. Failed direct uploads do not fall back to relay automatically. Direct S3 uploads bypass Ycloud's live rate limiter and all traffic accounting and quotas, regardless of the user's role. Permissions, file-size limits, and storage capacity limits still apply. Relay and WebDAV transfers are counted as file data passing through the server. Current S3 downloads are server-relayed and count toward traffic; being stored in S3 does not exempt them. Downloads redirected to a direct URL are not counted.

Use the official HTTPS endpoint for the bucket's Region with Alibaba Cloud OSS and Tencent Cloud COS. Only one Ycloud instance may write to a given Bucket/Prefix.

Configure an incomplete-multipart-upload lifecycle rule in the bucket console to remove empty sessions left by lost creation responses. Ycloud does not change bucket lifecycle rules.

### Deployment limits

The admin console can change application limits, but cannot exceed these deployment limits:

- `MAX_UPLOAD_BYTES`
- `MAX_UPLOAD_BATCH_BYTES`
- `MAX_UPLOAD_BATCH_ENTRIES`
- `MAX_ARCHIVE_BYTES`
- `MAX_ARCHIVE_ENTRIES`

On first start, each application default is the lower of its built-in default and the deployment limit. Existing configurations are not reduced automatically. Upload and archive byte limits must be at least 1 MiB; the batch byte limit must be at least the single-file upload limit.

## Checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked

cd frontend
npm run check
```

## Traffic quotas and accounting

- **Quotas:** site-wide, shared guest downloads, shared user-account traffic, and per-account traffic. Uploads and downloads are separate; `0` means unlimited.
- **Accounting:** the dashboard counts file data passing through the VPS. User uploads are inbound; user downloads are outbound. Direct S3 uploads and downloads redirected to a direct URL are excluded for every role. Server-relayed downloads, previews, and archives still count. Disabling quotas does not disable accounting. Administrator relay transfers count but are not quota-limited; independent WebDAV mounts are subject to the site-wide quota. These are application-level file bytes, not total network-interface traffic. System/API overhead and the extra backend connection used by S3 relay are not included.
- **Older data:** previous versions did not record whether traffic was direct or relayed, so old direct-upload traffic cannot be subtracted reliably. Upgrades retain existing data; new rules apply from the upgrade and use the existing reset cycle.
- **Resets and backups:** quotas reset on hourly, daily, or monthly cycles. Restarts do not reset usage. Stop the service before backing up, and save the configuration together with `traffic-usage.json` and `traffic-usage.jsonl`.

### Diagnose a damaged ledger

A damaged traffic ledger does not reset usage or disable quota enforcement. Stop the service, back up the ledger, and run `ycloud doctor traffic` with the same `CONFIG_PATH`. This is a read-only check: it does not repair the ledger or create files. Restore corrupt complete records or sequence gaps from a trusted backup. Do not delete the ledger and start over.

## Open-source components

Ycloud uses Vue, Phosphor Icons, Reka UI, Tokio, Axum, the AWS SDK for Rust, and other open-source components. See [Third-party notices](THIRD_PARTY_NOTICES.md) for the component list and required license texts.
