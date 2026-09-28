reject() {
    if "$@" >"$sandbox/rejected.out" 2>"$sandbox/rejected.err"; then
        printf 'Expected rejection\n' >&2
        exit 1
    fi
}
root="$test_home/devkit-game"
run_status
ln -s "$sandbox" "$root"
reject run_prepare
rm "$root"
run_prepare
run_prepare
printf keep > "$root/YugeHasi_contract_probe/keep"
run_prepare
test "$(cat "$root/YugeHasi_contract_probe/keep")" = keep
mv "$root/YugeHasi_contract_probe.yuge-hasi-owner" "$sandbox/owner"
reject run_prepare
mv "$sandbox/owner" "$root/YugeHasi_contract_probe.yuge-hasi-owner"
mkdir "$test_home/.config"
touch "$test_home/.config/inhibit-short-session-tracker"
reject run_prepare
reject run_register
rm "$test_home/.config/inhibit-short-session-tracker"
exec 7>>"$root/.yuge-hasi-lock"
flock -n 7
reject run_prepare
flock -u 7
exec 7>&-
cp /usr/bin/true "$root/YugeHasi_contract_probe/Game Name"
ln -s /usr/bin/true "$root/YugeHasi_contract_probe/link"
reject run_register
rm "$root/YugeHasi_contract_probe/link"
mkdir "$test_home/.steam"
printf '%s' "$$" > "$test_home/.steam/steam.pid"
printf fixture-token > "$test_home/.steam/steam.token"
mkfifo "$test_home/.steam/steam.pipe"
mock_steam() {
    IFS= read -r command < "$test_home/.steam/steam.pipe"
    case "$command" in 'devkit-1 steam://devkit-1/fixture-token/create-shortcut?response='*) ;; *) exit 1;; esac
    encoded=${command#*response=}
    encoded=${encoded%%&*}
    printf -v decoded '%b' "${encoded//%/\\x}"
    case "$decoded" in /tmp/yuge-hasi-register.*/response) ;; *) exit 1;; esac
    case "$1" in
        success) printf ok > "$decoded" ;;
        error) printf rejected > "$decoded.error" ;;
        locked) touch "$decoded.lock"; printf ok > "$decoded"; sleep 0.3; rm "$decoded.lock" ;;
        timeout) : ;;
    esac
}
mock_steam success & server=$!
run_register
wait "$server"
printf 'ARGV_BASE64='
base64 -w0 "$root/YugeHasi_contract_probe-argv.json"
printf '\n'
test "$(cat "$root/YugeHasi_contract_probe-settings.json")" = '{"compat_tool":"SteamLinuxRuntime_sniper","steam_play":"0"}'
mock_steam locked & server=$!
run_register
wait "$server"
mock_steam error & server=$!
reject run_register
wait "$server"
mock_steam timeout & server=$!
reject run_register
wait "$server"
reject run_register
mv "$root/YugeHasi_contract_probe-env.json" "$sandbox/env"
ln -s "$sandbox/env" "$root/YugeHasi_contract_probe-env.json"
reject run_register
test "$(cat "$sandbox/env")" = '{}'
test ! -e "$test_home/.local"
test ! -e "$test_home/devkit-utils"
printf 'REMOTE_CONTRACT_OK\n'
