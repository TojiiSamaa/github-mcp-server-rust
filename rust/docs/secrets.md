# Fournir un jeton par compte

Le fichier de configuration ne contient jamais de secret, seulement où le lire. Chaque compte `classic` ou `fine` accepte trois sources. Tous les noms ci-dessous sont fictifs.

| Clé | Contenu |
|---|---|
| `token_env` | nom d'une variable d'environnement qui contient le jeton |
| `token_file` | chemin d'un fichier qui contient le jeton |
| `token_cmd` | programme + arguments (tableau, sans shell) dont la sortie standard est le jeton |

Un compte `app` n'utilise pas ces clés : il lit `private_key_env` ou `private_key_file` (clé privée PEM) et échange un jeton d'installation.

## Ordre de résolution

1. `token_env`, si la variable existe et n'est pas vide.
2. `token_file`, s'il est présent (un fichier illisible est une erreur, la commande n'est pas essayée).
3. `token_cmd`, seulement si les deux autres ne fournissent rien.

Le jeton est lu au moment d'un vrai appel GitHub, jamais en mode test (`--test` coupe aussi les commandes). Aucune mise en cache : `token_cmd` est relancée à chaque appel, donc la commande doit rester rapide (coffre déjà déverrouillé).

## `token_cmd`

```toml
[[accounts]]
name = "alice-test"
kind = "fine"
token_cmd = ["programme", "argument1", "argument2"]
```

- Pas de shell : pas de `|`, de `$VAR`, de `~` ni de redirection. Pour un enchaînement, appeler explicitement un interpréteur (voir Windows plus bas).
- La sortie standard, débarrassée des espaces et retours à la ligne autour, est le jeton.
- Erreur si le programme est introuvable, si le code de sortie n'est pas 0, ou si la sortie est vide. Le message ne reprend jamais la sortie de la commande.
- Le jeton n'est jamais journalisé ; l'affichage de débogage de la configuration ne montre que le programme, pas les arguments.
- La commande hérite de l'environnement du processus `ghmcp` : une variable de session (`BW_SESSION`…) doit être définie là où le client MCP lance `ghmcp`.

## Recettes

### Bitwarden / Vaultwarden (`bw`)

```toml
token_cmd = ["bw", "get", "password", "<élément>"]
```

Déverrouiller d'abord (`bw unlock --raw`) et placer le résultat dans `BW_SESSION` pour le processus `ghmcp`. Sans session, `bw` échoue et le compte est en erreur.

### 1Password (`op`)

```toml
token_cmd = ["op", "read", "op://<coffre>/<élément>/<champ>"]
```

### KeePassXC

```toml
token_cmd = ["keepassxc-cli", "show", "-s", "-a", "Password", "<base.kdbx>", "<entrée>"]
```

`-s` affiche l'attribut sans le masquer. La commande demande le mot de passe de la base : prévoir un fichier clé ou un déverrouillage préalable, car aucune saisie n'est possible.

### pass

```toml
token_cmd = ["pass", "show", "<chemin/de/l'entrée>"]
```

`pass show` renvoie la première ligne en premier : y mettre le jeton seul.

### Gestionnaire d'identification Windows

Windows ne fournit pas de commande native pour lire un mot de passe ; passer par PowerShell et le module `CredentialManager` (`Install-Module CredentialManager`) :

```toml
token_cmd = ["powershell.exe", "-NoProfile", "-Command", "(Get-StoredCredential -Target '<cible>' -AsCredentialObject).Password"]
```

### Variables d'environnement

Sans `token_cmd` :

```toml
token_env = "GHMCP_ALICE_TOKEN"
```

La variable doit exister dans l'environnement du processus `ghmcp` (le bloc `env` du client MCP, ou le service qui le lance). Un fichier lu par `token_file` convient aux secrets montés par un orchestrateur.

## Bonnes pratiques

- Un compte par jeton, avec les permissions minimales : `gh_permissions_schema` dit ce que chaque outil exige, `gh_account_audit` ce que le jeton permet.
- Ne jamais écrire le jeton dans le TOML, dans un argument de `token_cmd` ni dans le dépôt.
- Les secrets de webhook et de jeton d'API suivent la même logique par variable : `webhook.secret_env`, `webhook.api_token_env`, `url_env` d'une destination.
