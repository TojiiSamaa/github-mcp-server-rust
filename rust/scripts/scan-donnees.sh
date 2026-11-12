#!/usr/bin/env bash
# Aucune donnée réelle dans le dépôt : sort vide (code 0) si tout est propre, sinon liste et code 1.
# Usage : scripts/scan-donnees.sh [dossier]   (défaut : la racine du dépôt)
# Motifs génériques : IP privées, MAC, chemins de poste, e-mails, jetons GitHub non FAKE.
# Motifs personnels (noms réels) : jamais dans le dépôt. Ils viennent de $SCAN_NOMS (regex) et/ou du fichier
# ignoré .scan-noms.local à la racine (une regex par ligne, # = commentaire).
# Exemptions : boucle locale et plages de documentation (192.0.2/198.51.100/203.0.113), jetons contenant FAKE,
# e-mails noreply / domaines d'exemple, et l'URL publique du dépôt (github.com/<compte>/github-mcp-server-rust).
RACINE="$(cd "$(dirname "$0")/.." && pwd)"
cd "${1:-$RACINE}" || exit 2
EXCL=(--hidden -g '!target' -g '!Cargo.lock' -g '!schemas/official' -g '!.git' -g '!.scan-noms.local' -g '!.github/workflows/prod.yml')   # prod.yml (livraison privée) est retiré par publier.sh : il n'est jamais publié
P_IP='\b((25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])\b'
P_MAC='\b([0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b'
P_PATH='[A-Za-z]:[\\/]+Users[\\/]|/home/[a-z][a-z0-9_-]+|/Users/[A-Za-z]'
P_MAIL='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
P_TOK='(ghp_|gho_|ghs_|ghu_|github_pat_)[[:alnum:]_]{6,}'   # scan:ok
NOMS="${SCAN_NOMS:-}"
if [ -f "$RACINE/.scan-noms.local" ]; then
  while IFS= read -r m; do m="${m%$'\r'}"; [ -n "$m" ] && [ "${m#\#}" = "$m" ] && NOMS="${NOMS:+$NOMS|}$m"; done <"$RACINE/.scan-noms.local"
fi
OUT=$(
  rg -n "${EXCL[@]}" "$P_IP" . | rg -v '\b(127\.0\.0\.1|0\.0\.0\.0|192\.0\.2\.[0-9]+|198\.51\.100\.[0-9]+|203\.0\.113\.[0-9]+)\b'
  rg -n "${EXCL[@]}" "$P_MAC" .
  rg -n "${EXCL[@]}" "$P_PATH" . | rg -v '/home/(user|exemple|example|runner)\b'
  rg -n -o "${EXCL[@]}" "$P_MAIL" . | rg -v -i 'noreply|@example\.|\.example$|\.invalid$|\.test$|:git@'
  rg -n "${EXCL[@]}" "$P_TOK" . | rg -v -i 'fake'
  # l'URL publique du dépôt est retirée de la ligne avant de chercher les noms (le reste de la ligne compte)
  if [ -n "$NOMS" ]; then
    rg -n -i "${EXCL[@]}" "$NOMS" . | sed -E 's#github\.com/[A-Za-z0-9-]+/github-mcp-server-rust##g' | rg -i "^[^:]+:[0-9]+:.*($NOMS)" \
      | rg -v 'uses: [A-Za-z0-9-]+/ci-deploy/|ghcr\.io/[a-z0-9-]+/github-mcp-rust'   # plomberie CI du circuit privé : référence d'un dépôt/image, pas une donnée personnelle
  fi
)
if [ -n "$OUT" ]; then echo "$OUT"; exit 1; fi
