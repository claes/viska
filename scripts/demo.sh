layout=${VISKA_DEMO_LAYOUT:-us}
case "$layout" in us|se) ;; *) echo 'Layout must be us or se' >&2; exit 1;; esac
work=$(mktemp -d -t viska-demo.XXXXXXXX)
daemon_pid=
cleanup() {
  if [ -n "$daemon_pid" ]; then kill "$daemon_pid" 2>/dev/null || true; wait "$daemon_pid" || true; fi
  rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
user=$(id -un)
jq -n --arg user "$user" --arg dir "$work" --arg layout "$layout" '{
  keyboard_layout:$layout, backend:"mock",
  submission_socket:($dir+"/submit.sock"),control_socket:($dir+"/control.sock"),
  authorization:{store_users:[$user],type_users:[$user],clear_users:[$user],status_users:[$user]}
}' > "$work/config.json"
viskad --config "$work/config.json" &
daemon_pid=$!
export VISKA_SOCKET_DIR="$work"
for _ in $(seq 1 100); do
  if viskactl status >/dev/null 2>&1; then break; fi
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo 'Demo startup failed. Check the error above and ulimit -l (at least 4096 KiB recommended).' >&2
    exit 1
  fi
  sleep 0.05
done
viskactl status
printf 'Mock demo (%s): use viskactl store/type/clear/status. Nothing is typed into host applications.\n' "$layout"
echo 'Exit the shell to stop the daemon and remove its private runtime directory.'
bash --noprofile --norc -i
