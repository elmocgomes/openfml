# Installing OpenFML

One binary is the whole deployment: `openfml-server <config-dir> <port>`
serves the **Budget Portal** at `/`, the **modelling Studio** at
`/studio`, and the API — one port, no other processes. All state lives
in the config directory (`users.json` with accounts and roles,
`access.cfg`, `models/`, `logs/` with the signed audit chains,
`server.secret`); the binaries are stateless, which is what makes
upgrades safe.

On first boot against a fresh config directory the server **seeds a
Super Admin** (`admin`), prints its one-time password to the console
and writes it to `<config-dir>/admin-initial-password.txt` (mode 0600 —
delete it after first login). Sign in with it at the portal or studio;
you are forced to choose your own password, then manage every other
user and role from **Govern → People & access** (portal) or the
**Team** page (studio). A legacy `users.cfg` is migrated into
`users.json` automatically.

## 1 · From a release bundle (servers and laptops, no toolchain)

```bash
tar -xzf openfml-<version>-<target>.tar.gz
cd openfml-<version>-<target>
./install.sh              # user install → ~/.local
# or:  sudo ./install.sh system   → /usr/local/bin + /opt/openfml
```

Then:

```bash
openfml-server ~/.local/share/openfml/deploy 8080
```

- Portal (contributors): `http://localhost:8080/` — sign in with
  username and password (or an access token).
- Studio (modellers): `http://localhost:8080/studio` — same login;
  without a server it runs as a local sandbox.
- Headless tokens (CI, scripts):
  `openfml-server token <user> <config-dir>/server.secret`

Bundles are produced by `scripts/package.sh` (per platform: build on the
platform you target).

## 2 · With cargo (anyone with Rust)

```bash
cargo install --path .          # from a checkout
# or, once hosted:  cargo install --git https://github.com/elmocgomes/openfml openfml
```

This installs `openfml`, `openfml-server` and `openfml-lsp`. The server
looks for the web UI in `<config-dir>/www` then `./www`, so run it from
the checkout or copy `www/` next to your config directory.

## 3 · Docker (servers)

```bash
docker build -t openfml .
docker run -d --name openfml -p 8080:8080 -v openfml-data:/data openfml
```

The volume `/data` is the config directory. First run seeds it from the
template; after that your users, models and signed logs persist across
container replacements.

## Setting up a multi-user deployment

1. Sign in as the seeded Super Admin and create users under
   **Govern → People & access** (portal) or **Team** (studio): username,
   department, role, and optionally an initial password (the user
   replaces it on first login). Custom roles combine a base capability
   (`admin` | `editor` | `viewer`) with the *manages users* flag — the
   Super Admin capability.
2. Edit `<config-dir>/access.cfg` — per model: readable departments and
   per-department write grants (grants reach only literal input cells;
   formulas are admin-only, structurally). Departments on accounts must
   match `access.cfg`.
3. Put your `.fml` model files (and any `data` CSV facts) in
   `<config-dir>/models/`, restart, and send each person their login.

Security posture: the server binds localhost and speaks plain HTTP;
tokens are bearer credentials. For anything beyond one machine, front it
with a TLS reverse proxy (nginx/caddy) — see
`scripts/openfml-server.service` for the systemd unit.

## Updating to a new version

The contract: **state and binaries are separate.** Upgrading never
touches the config directory.

1. (Recommended) have an admin run **Checkpoint** first — approved
   numbers land in the model files and the signed log archives cleanly.
2. Replace the binaries:
   - bundle: unpack the new one, re-run `./install.sh` (same flavor);
   - cargo: `cargo install --path . --force` (or `--git … --force`);
   - docker: `docker pull`/rebuild, `docker rm` the container, `run`
     again **with the same volume**.
3. Restart. The server replays the signed logs against your models on
   boot — if a release ever changes the log format it will refuse to
   start with a clear message rather than guess (that is the point of
   the signatures).

Checking what's running: `openfml --version`,
`openfml-server --version`, the `version` field on `GET /models`, and
the version shown in the portal sidebar / studio identity chip.
