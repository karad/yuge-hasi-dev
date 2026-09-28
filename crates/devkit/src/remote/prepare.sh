lock_root
marker="$root/$title.yuge-hasi-owner"
if exists "$marker"; then
    regular "$marker"
    test "$(cat -- "$marker")" = yuge-hasi-devkit-v1 || fail 'Title is not owned by this client'
else
    for suffix in '' -argv.json -env.json -settings.json; do
        if exists "$root/$title$suffix"; then fail 'Existing title is not managed by this client; choose a new title ID'; fi
    done
    temporary=$(mktemp "$root/.yuge-hasi-owner.XXXXXXXX")
    trap 'rm -f -- "$temporary"' EXIT
    printf 'yuge-hasi-devkit-v1\n' > "$temporary"
    mv -- "$temporary" "$marker"
fi
directory "$root/$title"
printf '{"protocol":2,"prepared":true}\n'
