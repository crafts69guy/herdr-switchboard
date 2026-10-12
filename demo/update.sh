#!/usr/bin/env bash
# A canned managed update inside the recording sandbox; no installer/network runs.
set -euo pipefail
SANDBOX="${HOME%/home}"
case "$SANDBOX" in
  /tmp/* | /private/tmp/*) ;;
  *) printf 'update demo: run through demo/enter.sh\n' >&2; exit 1 ;;
esac
[[ "$HOME" == "$SANDBOX/home" && -x "$SANDBOX/bin/switchboard-update-demo" ]] || {
  printf 'update demo: recording sandbox is missing\n' >&2
  exit 1
}
root="$SANDBOX/update-plugin"
mkdir -p "$root/bin" "$root/tools"
cp "$SANDBOX/bin/switchboard-update-demo" "$root/switchboard"
cat >"$root/herdr-plugin.toml" <<'EOF'
id = "switchboard-update-demo"
name = "Switchboard update demo"
version = "1.0.0"
min_herdr_version = "0.8.0"
description = "Disposable update recording fixture"
[[panes]]
id = "update"
title = "󰏔"
placement = "overlay"
command = ["bash", "-c", "export PATH=\"$HERDR_PLUGIN_ROOT/tools:$PATH\"; export HERDR_BIN_PATH=\"$HERDR_PLUGIN_ROOT/tools/update-herdr\"; exec \"$HERDR_PLUGIN_ROOT/switchboard\" --update"]
EOF
cat >"$root/tools/update-herdr" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2" == "plugin list" ]]; then
  printf '{"result":{"plugins":[{"plugin_id":"switchboard","plugin_root":"%s","version":"9.9.9","source":{"kind":"github","owner":"crafts69guy","repo":"herdr-switchboard","managed_path":"%s"}}]}}\n' "$HERDR_PLUGIN_ROOT" "$HERDR_PLUGIN_ROOT"
elif [[ "$1 $2" == "plugin install" ]]; then
  [[ "$3 $4 $5 $6" == "crafts69guy/herdr-switchboard --yes --ref v9.9.9" ]]
  sleep 2
else
  exit 2
fi
EOF
cat >"$root/tools/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "ls-remote --tags --refs https://github.com/crafts69guy/herdr-switchboard" ]]
sleep 0.3
printf 'abcdef\trefs/tags/v9.9.9\n'
EOF
cat >"$root/bin/picker.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == "--version" ]]
sleep 2
printf 'herdr-switchboard 9.9.9\n'
EOF
chmod 755 "$root/tools/update-herdr" "$root/tools/git"
herdr plugin link "$root" >/dev/null
config_dir="$(herdr plugin config-dir switchboard-update-demo)"
printf '[common]\nnotifications = false\n' >"$config_dir/config.toml"
herdr plugin pane open --plugin switchboard-update-demo --entrypoint update --placement overlay >/dev/null
