#!/bin/sh
# Install OpenFML from an unpacked release bundle.
#   ./install.sh              → user install (~/.local)
#   sudo ./install.sh system  → system install (/usr/local + /opt/openfml)
# State never lives with the binaries: a server's config directory
# (users.cfg, access.cfg, models/, logs/, server.secret) is yours and
# survives every upgrade. Re-running this script upgrades in place.
set -e
cd "$(dirname "$0")"

if [ "$1" = "system" ]; then
  BIN=/usr/local/bin
  SHARE=/opt/openfml
else
  BIN="$HOME/.local/bin"
  SHARE="$HOME/.local/share/openfml"
fi

mkdir -p "$BIN" "$SHARE"
cp bin/openfml bin/openfml-server bin/openfml-lsp "$BIN/"
rm -rf "$SHARE/www" "$SHARE/examples"
cp -R www "$SHARE/www"
cp -R examples "$SHARE/examples"
[ -d "$SHARE/deploy" ] || cp -R deploy-template "$SHARE/deploy"
cp openfml-server.service "$SHARE/" 2>/dev/null || true

echo "installed $(bin/openfml --version) → $BIN"
echo
echo "next steps:"
echo "  local sandbox (this machine only):"
echo "    cd $SHARE && openfml-server deploy 8080     # then open http://localhost:8080/studio"
echo "  multi-user server:"
echo "    edit $SHARE/deploy/users.cfg and access.cfg, restart the server,"
echo "    mint tokens:  openfml-server token <user> $SHARE/deploy/server.secret"
echo "    portal: http://localhost:8080/   studio: http://localhost:8080/studio?token=…"
echo "  upgrading later: unpack the new bundle and re-run this script —"
echo "    your deploy/ directory (models, logs, secret, users) is untouched."
