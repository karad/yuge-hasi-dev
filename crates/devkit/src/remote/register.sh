lock_root
owned
unexpected=$(find "$root/$title" ! -type d ! -type f -print -quit)
test -z "$unexpected" || fail 'Links and special files are not supported'
regular "$root/$title/$executable"
if [[ "$runtime" == android ]]; then
    [[ "$(uname -m)" == aarch64 ]] || fail 'Android APK requires an ARM64 device'
    header=$(od -An -tx1 -N4 -- "$root/$title/$executable" | tr -d ' \n')
    [[ "$header" == 504b0304 ]] || fail 'Expected an APK archive'
else
    [[ "$(uname -m)" == x86_64 ]] || fail 'Linux build requires an x86_64 device'
    header=$(od -An -tx1 -N20 -- "$root/$title/$executable" | tr -d ' \n')
    [[ "${header:0:14}" == 7f454c46020101 && "${header:36:4}" == 3e00 ]] || fail 'Expected a Linux ELF64 x86_64 executable'
fi
steam="$device_home/.steam"
pid=$(cat "$steam/steam.pid")
[[ "$pid" =~ ^[0-9]+$ ]] && test "$pid" -gt 1 && kill -0 "$pid" || fail 'Steam is not running'
token=$(cat "$steam/steam.token")
[[ "$token" =~ ^[a-zA-Z0-9_-]+$ ]] && test "${#token}" -le 512 || fail 'Unexpected Steam token format'
test -p "$steam/steam.pipe" || fail 'Steam IPC path is not a FIFO'
# Steam accepts response files under /tmp, not under the game directory.
response_dir=$(mktemp -d /tmp/yuge-hasi-register.XXXXXXXX)
temporary=''
trap 'rm -rf -- "$response_dir"; if test -n "$temporary"; then rm -f -- "$temporary"; fi' EXIT
for name in argv env settings; do
    destination="$root/$title-$name.json"
    if exists "$destination"; then regular "$destination"; fi
    variable="${name}_data"
    temporary=$(mktemp "$root/.yuge-hasi-settings.XXXXXXXX")
    printf '%s' "${!variable}" | base64 --decode > "$temporary"
    mv -- "$temporary" "$destination"
    temporary=''
done
if [[ "$runtime" != android ]]; then chmod 700 -- "$root/$title/$executable"; fi
response="$response_dir/response"
encoded=''
for ((i=0; i<${#response}; i++)); do
    byte=${response:i:1}
    case "$byte" in
        [a-zA-Z0-9.~_-]) encoded+="$byte" ;;
        *) printf -v hex '%%%02X' "'$byte"; encoded+="$hex" ;;
    esac
done
command="devkit-1 steam://devkit-1/$token/create-shortcut?response=$encoded&gameid=$title"
timeout 5 /bin/bash --noprofile --norc -c 'printf "%s\n" "$2" > "$1"' bash "$steam/steam.pipe" "$command" || fail 'Steam IPC write failed or timed out'
for ((attempt=0; attempt<100; attempt++)); do
    if ! exists "$response.lock"; then
        if exists "$response.error"; then fail 'Steam rejected registration'; fi
        if exists "$response"; then
            regular "$response"
            exec 8<"$response"
            flock -s -w 2 8 || fail 'Steam response is locked'
            test "$(stat -c %s -- "$response")" -le 65536 || fail 'Steam response is too large'
            printf '{"protocol":2,"registered":true,"launch_verified":false}\n'
            exit 0
        fi
    fi
    sleep 0.1
done
fail 'Steam registration timed out; game files remain available for retry'
