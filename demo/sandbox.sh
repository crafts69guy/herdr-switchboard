#!/usr/bin/env bash
# Build a disposable Herdr world for recording the README demos.
#
# Everything a demo shows comes from here: a fake $HOME, a ghq root of invented
# repositories, a seeded shell history, stand-in agents, canned quota readings,
# and Herdr + Switchboard configs. Nothing is read from the recording machine, so
# no real repository, path, account, or command can reach a published GIF.
#
# The root must stay short: Herdr's socket lives under it and a Unix socket path
# is capped at ~104 bytes, which a scratch directory under $TMPDIR exceeds.
set -euo pipefail

DEMO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd -- "$DEMO_DIR/.." && pwd)"
SANDBOX="${SWB_DEMO_ROOT:-/tmp/swb-demo}"
DEMO_HOME="$SANDBOX/home"
SRC="$DEMO_HOME/src/github.com"
WORKTREES="$DEMO_HOME/.herdr/worktrees"
NOW="$(date +%s)"

case "$SANDBOX" in
  /tmp/* | /private/tmp/*) ;;
  *)
    printf 'sandbox: refusing to rebuild %s (outside /tmp)\n' "$SANDBOX" >&2
    exit 1
    ;;
esac

# Run anything Herdr- or git-shaped as the sandbox, never as the recording user.
in_sandbox() {
  SWB_DEMO_CWD="$DEMO_HOME" "$DEMO_DIR/enter.sh" "$@"
}

# An ISO-8601 UTC timestamp N seconds from now, on BSD or GNU date.
iso_in() {
  local at=$((NOW + $1))
  date -u -r "$at" +%Y-%m-%dT%H:%M:%S+00:00 2>/dev/null ||
    date -u -d "@$at" +%Y-%m-%dT%H:%M:%S+00:00
}

b64url() { printf '%s' "$1" | base64 | tr '+/' '-_' | tr -d '=\n'; }

# One commit, dated so the Inspector's "last commit" reads naturally.
commit() {
  local repo="$1" days_ago="$2" message="$3"
  local stamp=$((NOW - days_ago * 86400 - 1800))
  in_sandbox git -C "$repo" add -A
  in_sandbox env GIT_AUTHOR_DATE="@$stamp" GIT_COMMITTER_DATE="@$stamp" \
    git -C "$repo" commit --quiet --allow-empty -m "$message"
}

# repo <owner/name> <description> <file>... — each file gets one line of content
# so the Inspector's tree has something to show.
repo() {
  local slug="$1" description="$2"
  shift 2
  local dir="$SRC/$slug" file
  mkdir -p "$dir"
  in_sandbox git init --quiet -b main "$dir"
  printf '# %s\n\n%s\n' "${slug#*/}" "$description" >"$dir/README.md"
  for file in "$@"; do
    mkdir -p "$dir/$(dirname -- "$file")"
    printf '// %s\n' "$file" >"$dir/$file"
  done
  commit "$dir" 21 "Initial commit"
}

# A server left over from an earlier take holds the old socket; stop it first.
if [[ -S "$DEMO_HOME/.config/herdr/herdr.sock" ]]; then
  in_sandbox herdr server stop >/dev/null 2>&1 || true
fi
rm -rf -- "$SANDBOX"
mkdir -p "$SANDBOX"
# Resolve /tmp -> /private/tmp up front: Herdr reports physical cwds, and the
# Inspector only abbreviates a path to ~ when it starts with $HOME verbatim.
SANDBOX="$(cd -- "$SANDBOX" && pwd -P)"
DEMO_HOME="$SANDBOX/home"
SRC="$DEMO_HOME/src/github.com"
WORKTREES="$DEMO_HOME/.herdr/worktrees"
mkdir -p "$DEMO_HOME/.config/herdr" "$DEMO_HOME/.local/state" "$DEMO_HOME/.local/share" \
  "$SRC" "$SANDBOX/bin" "$SANDBOX/libexec"

cat >"$DEMO_HOME/.gitconfig" <<EOF
[user]
	name = Demo User
	email = demo@example.com
[init]
	defaultBranch = main
[ghq]
	root = $DEMO_HOME/src
[advice]
	detachedHead = false
EOF

# --- Repositories --------------------------------------------------------------

repo northwind/api-gateway "Edge gateway: auth, rate limits, and routing for every service." \
  cmd/gateway/main.go internal/auth/jwt.go internal/ratelimit/bucket.go go.mod Makefile
commit "$SRC/northwind/api-gateway" 6 "Add token bucket rate limiter"
commit "$SRC/northwind/api-gateway" 1 "Cache JWKS keys between requests"

repo northwind/web-dashboard "Operator dashboard for the Northwind platform." \
  src/app/page.tsx src/components/Chart.tsx src/lib/api.ts package.json tsconfig.json
printf '20.11.1\n' >"$SRC/northwind/web-dashboard/.nvmrc"
commit "$SRC/northwind/web-dashboard" 4 "Add latency chart to the overview"
commit "$SRC/northwind/web-dashboard" 0 "Paginate the incidents table"

repo northwind/mobile-app "React Native client for field technicians." \
  app/index.tsx app/settings.tsx src/hooks/useSync.ts package.json app.json
commit "$SRC/northwind/mobile-app" 3 "Retry offline sync with backoff"

repo northwind/infra "Terraform for every Northwind environment." \
  modules/vpc/main.tf modules/eks/main.tf envs/prod/main.tf envs/staging/main.tf
commit "$SRC/northwind/infra" 9 "Bump EKS node group to 1.31"

repo northwind/design-system "Tokens, primitives, and the component library." \
  tokens/color.json src/Button.tsx src/Dialog.tsx package.json
commit "$SRC/northwind/design-system" 12 "Add focus ring tokens"

repo northwind/billing-worker "Invoices, retries, and dunning — one queue consumer." \
  src/main.rs src/invoice.rs src/retry.rs Cargo.toml
commit "$SRC/northwind/billing-worker" 2 "Make dunning retries idempotent"

repo ada/dotfiles "Shell, editor, and terminal configuration." \
  .config/fish/config.fish .config/nvim/init.lua .config/herdr/config.toml
repo ada/notes "Plain-text notes and drafts." \
  inbox.md ideas/terminal-first.md
repo ada/raytracer "A weekend ray tracer in Rust." \
  src/main.rs src/vec3.rs src/camera.rs Cargo.toml
commit "$SRC/ada/raytracer" 30 "Add depth of field"

# A linked worktree, so the Worktrees group is not empty. It lives outside the
# ghq root, the way Herdr places them, or ghq would list it as a repository too.
in_sandbox git -C "$SRC/northwind/web-dashboard" worktree add --quiet \
  -b feat/dark-mode "$WORKTREES/web-dashboard/dark-mode"
printf ':root { color-scheme: dark; }\n' >"$WORKTREES/web-dashboard/dark-mode/src/app/dark.css"
commit "$WORKTREES/web-dashboard/dark-mode" 0 "Sketch the dark palette"

# Uncommitted work in the gateway, so the Git menu has a review to offer.
cat >>"$SRC/northwind/api-gateway/internal/ratelimit/bucket.go" <<'EOF'

// Allow spends one token, refilling first from the time elapsed.
func (b *Bucket) Allow(now time.Time) bool {
	b.refill(now)
	if b.tokens < 1 {
		return false
	}
	b.tokens--
	return true
}
EOF
cat >>"$SRC/northwind/api-gateway/internal/auth/jwt.go" <<'EOF'

// keyTTL bounds how long a fetched JWKS key is trusted without a refetch.
const keyTTL = 10 * time.Minute
EOF

# --- Shell ---------------------------------------------------------------------

cat >"$DEMO_HOME/.bashrc" <<'EOF'
export BASH_SILENCE_DEPRECATION_WARNING=1
export HISTFILE="$HOME/.bash_history"
PS1='\[\e[34m\]\W\[\e[0m\] \[\e[32m\]❯\[\e[0m\] '
EOF
printf '[ -f ~/.bashrc ] && . ~/.bashrc\n' >"$DEMO_HOME/.bash_profile"

history=(
  "cargo test --workspace"
  "make run PORT=8080"
  "go test ./internal/..."
  "pnpm dev --port 3000"
  "pnpm test --watch"
  "terraform plan -out=tfplan"
  "kubectl -n staging get pods"
  "kubectl -n staging logs deploy/api-gateway -f"
  "docker compose up -d postgres redis"
  "git log --oneline --graph -20"
  "git switch -c feat/dark-mode"
  "gh pr create --fill"
  "rg -n 'TODO|FIXME' src"
  "pnpm lint --fix"
  "cargo run --release -- --scene demo.toml"
  "curl -s localhost:8080/healthz"
  "terraform apply tfplan"
)
for i in "${!history[@]}"; do
  printf '#%s\n%s\n' "$((NOW - 3600 * (${#history[@]} - i)))" "${history[$i]}"
done >"$DEMO_HOME/.bash_history"

# --- Stand-ins -------------------------------------------------------------------
#
# Herdr recognises an agent by its process name, so a tiny binary that is merely
# *named* after one is enough to populate the Agents group. (A renamed copy of a
# system binary will not do: macOS kills it for its broken platform signature.)
# Its banner says plainly that it is a stand-in — no agent CLI, account, or
# network is involved.
cat >"$SANDBOX/libexec/stand-in.c" <<'EOF'
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
  const char *name = strrchr(argv[0], '/');
  name = name ? name + 1 : argv[0];
  (void)argc;
  printf("\n  \033[1m%s\033[0m  \033[2mdemo stand-in, no model behind this pane\033[0m\n\n  \033[35m>\033[0m ", name);
  fflush(stdout);
  for (;;) pause();
}
EOF
for agent in claude codex; do
  cc -O2 -o "$SANDBOX/bin/$agent" "$SANDBOX/libexec/stand-in.c"
done

# Tapes dress each take from inside Herdr, where the repository path is gone.
install -m 755 "$DEMO_DIR/stage.sh" "$SANDBOX/bin/stage"

# Usage reads quota exactly where the real providers leave it. Codex's numbers
# come off disk; Claude's come from the keychain plus one HTTPS request, so both
# of those commands are stubbed on the sandbox PATH and answer canned JSON.
day="$(date -u +%Y/%m/%d)"
mkdir -p "$DEMO_HOME/.codex/sessions/$day"
claims="{\"email\":\"ada@example.com\",\"https://api.openai.com/auth\":{\"chatgpt_plan_type\":\"plus\",\"chatgpt_subscription_active_until\":\"$(iso_in $((12 * 86400)))\"}}"
printf '{"auth_mode":"chatgpt","tokens":{"id_token":"%s.%s.demo"}}\n' \
  "$(b64url '{"alg":"none"}')" "$(b64url "$claims")" >"$DEMO_HOME/.codex/auth.json"
# A real rollout opens with a session_meta line, and the reader skips the first
# line of whatever tail it reads, so the fixture needs one too.
{
  printf '{"timestamp":"%s","type":"session_meta","payload":{"cwd":"%s"}}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" "$SRC/northwind/web-dashboard"
  printf '{"timestamp":"%s","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":148210},"model_context_window":258400,"total_token_usage":{"cached_input_tokens":8120400,"input_tokens":8640120,"total_tokens":8702311}},"rate_limits":{"primary":{"used_percent":38.0,"window_minutes":300,"resets_at":%s},"secondary":{"used_percent":61.0,"window_minutes":10080,"resets_at":%s},"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"plus"}}}\n' \
  "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" "$((NOW + 2 * 3600 + 840))" "$((NOW + 3 * 86400 + 7200))"
} >"$DEMO_HOME/.codex/sessions/$day/rollout-demo.jsonl"

printf '{"oauthAccount":{"emailAddress":"ada@example.com","organizationType":"claude_max"}}\n' \
  >"$DEMO_HOME/.claude.json"
cat >"$SANDBOX/bin/security" <<'EOF'
#!/usr/bin/env bash
printf '{"claudeAiOauth":{"accessToken":"demo-token"}}\n'
EOF
cat >"$SANDBOX/bin/curl" <<EOF
#!/usr/bin/env bash
cat >/dev/null
printf '{"five_hour":{"utilization":72.0,"resets_at":"$(iso_in $((3 * 3600 + 1500)))"},"seven_day":{"utilization":44.0,"resets_at":"$(iso_in $((4 * 86400)))"},"seven_day_opus":null,"seven_day_sonnet":null}\n'
EOF
chmod +x "$SANDBOX/bin/security" "$SANDBOX/bin/curl"

# --- Herdr -----------------------------------------------------------------------

# The theme is Huế Mưa from hue-theme (https://github.com/crafts69guy/hue-theme),
# pinned here by value so every contributor renders the same frames. The matching
# terminal palette lives in demo/tapes/common.tape.
cat >"$DEMO_HOME/.config/herdr/config.toml" <<'EOF'
onboarding = false

[theme]
name = "terminal"

[theme.custom]
accent = "#00CF6A"
panel_bg = "#0A2E52"
surface0 = "#123F6E"
surface1 = "#4E7AA6"
surface_dim = "#123F6E"
overlay0 = "#6B819A"
overlay1 = "#8CA3BD"
text = "#E5F4FF"
subtext0 = "#9DB4D4"
mauve = "#8A7BFF"
green = "#00CF6A"
yellow = "#FF8D00"
red = "#FF5A63"
blue = "#10B6F8"
teal = "#10B6F8"
peach = "#FF8D00"

[ui]
accent = "#00CF6A"
pane_borders = "auto"
pane_outer_borders = false
pane_gaps = true
status_indicators = "dots"
show_agent_labels_on_pane_borders = false

[terminal]
default_shell = "/bin/bash"
shell_mode = "login"

[update]
version_check = false
manifest_check = false

[keys]
prefix = "ctrl+b"

[[keys.command]]
key = "prefix+space"
type = "plugin_action"
command = "switchboard.menu"

[[keys.command]]
key = "prefix+p"
type = "plugin_action"
command = "switchboard.projects"

[[keys.command]]
key = "prefix+r"
type = "plugin_action"
command = "switchboard.commands"

[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "switchboard.git"

[[keys.command]]
key = "prefix+u"
type = "plugin_action"
command = "switchboard.usage"

[[keys.command]]
key = "prefix+z"
type = "plugin_action"
command = "switchboard.zen-toggle"
EOF

in_sandbox herdr plugin link "$PLUGIN_ROOT" >/dev/null
config_dir="$(in_sandbox herdr plugin config-dir switchboard)"
mkdir -p "$config_dir"
cat >"$config_dir/config.toml" <<'EOF'
[common]
update_check = false
notifications = false

[[commands.presets]]
label = "Run the gateway locally"
command = "make run PORT=8080"
cwd = "origin"
EOF

printf 'sandbox: ready at %s\n' "$SANDBOX"
