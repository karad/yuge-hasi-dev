set -euo pipefail
export LC_ALL=C
umask 077
device_home=$1
root="$device_home/devkit-game"
fail() { printf '%s\n' "$1" >&2; exit 1; }
exists() { test -e "$1" || test -L "$1"; }
regular() { test -f "$1" && test ! -L "$1" || fail 'Expected a regular file'; }
directory() {
    test ! -L "$1" || fail 'Directory must not be a link'
    if ! test -d "$1"; then mkdir -m 700 -- "$1"; fi
}
check_sentinel() {
    if exists "$device_home/.config/inhibit-short-session-tracker"; then
        fail 'Review the existing session-tracker sentinel before upload'
    fi
}
lock_root() {
    check_sentinel
    directory "$root"
    if exists "$root/.yuge-hasi-lock"; then regular "$root/.yuge-hasi-lock"; fi
    exec 9>>"$root/.yuge-hasi-lock"
    flock -n 9 || fail 'Another device operation is active'
}
owned() {
    regular "$root/$title.yuge-hasi-owner"
    test "$(cat -- "$root/$title.yuge-hasi-owner")" = yuge-hasi-devkit-v1 || fail 'Title is not owned by this client'
    test -d "$root/$title" && test ! -L "$root/$title" || fail 'Invalid game directory'
}
