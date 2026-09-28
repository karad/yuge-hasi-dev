test "$(uname -s)" = Linux || fail 'Expected Linux'
architecture=$(uname -m)
case "$architecture" in
    x86_64|aarch64) ;;
    *) fail 'Expected Linux x86_64 or aarch64' ;;
esac
for tool in bash rsync base64 flock timeout mktemp find od stat; do
    command -v "$tool" >/dev/null || fail "Missing standard command: $tool"
done
sentinel=false
if exists "$device_home/.config/inhibit-short-session-tracker"; then sentinel=true; fi
steam_ready=false
if test -f "$device_home/.steam/steam.pid" && test -f "$device_home/.steam/steam.token" && test -p "$device_home/.steam/steam.pipe"; then
    pid=$(cat "$device_home/.steam/steam.pid")
    if [[ "$pid" =~ ^[0-9]+$ ]] && test "$pid" -gt 1 && kill -0 "$pid" 2>/dev/null; then steam_ready=true; fi
fi
home_base64=$(printf '%s' "$device_home" | base64 | tr -d '\n')
printf '{"protocol":2,"authenticated":true,"architecture":"%s","helper_required":false,"steam_ready":%s,"inhibit_sentinel_present":%s,"home_base64":"%s"}\n' "$architecture" "$steam_ready" "$sentinel" "$home_base64"
