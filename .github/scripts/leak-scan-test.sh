#!/usr/bin/env bash
# Tests for leak-scan.sh against crafted diffs. Run from anywhere: .github/scripts/leak-scan-test.sh
#
# Every leak below is assembled at run time from two halves, so this file adds no line the
# scanner would flag. The names and addresses are invented.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
scan="$here/leak-scan.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/leak-scan-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT

home_linux="/ho""me/zed-q/"
home_mac="/Use""rs/zed-q/"
bare_linux="/ho""me/zed-q"
bare_mac="/Use""rs/zed-q"
upstream_author="/ho""me/aram/"
lan_10="10.""20.30.40"
lan_172="172.""20.1.2"
lan_192="192.""168.50.60"
cgnat="100.""101.102.103"
tailnet="box.example-net.t""s.net"
below_cgnat="100.""63.1.1"
above_private="172.""32.0.1"

failures=0
check() {
    local name=$1 want=$2 diff_file=$3
    local out status=0
    out="$("$scan" --diff "$diff_file" 2>/dev/null)" || status=$?
    if [[ $status -ne $want ]]; then
        echo "FAIL $name: exit $status, want $want"
        echo "$out" | sed 's/^/    /'
        failures=$((failures + 1))
        return
    fi
    echo "ok   $name"
    LAST_OUT=$out
}

# One added line per leak kind, plus the same leaks on removed and context lines.
cat >"$work/leaks.diff" <<EOF
diff --git a/docs/notes.md b/docs/notes.md
index 1111111..2222222 100644
--- a/docs/notes.md
+++ b/docs/notes.md
@@ -1,2 +7,7 @@
-old line with $home_linux that is being removed
+config lives in ${home_linux}.config
+and on a Mac in ${home_mac}Library
+ssh to $lan_10 or $lan_172 or $lan_192
+the tailnet address $cgnat
+serve it on https://$tailnet/
+nothing to see on this line
+++ a line whose text starts with two plus signs, at $lan_10
EOF
check "every leak kind on an added line fails" 1 "$work/leaks.diff"
for want in \
    "docs/notes.md:7: home path: $home_linux" \
    "docs/notes.md:8: home path: $home_mac" \
    "docs/notes.md:9: private IPv4 address: $lan_10" \
    "docs/notes.md:9: private IPv4 address: $lan_172" \
    "docs/notes.md:9: private IPv4 address: $lan_192" \
    "docs/notes.md:10: Tailscale CGNAT address: $cgnat" \
    "docs/notes.md:11: tailnet name: $tailnet" \
    "docs/notes.md:13: private IPv4 address: $lan_10"; do
    if ! grep -qxF "$want" <<<"$LAST_OUT"; then
        echo "FAIL missing hit: $want"
        failures=$((failures + 1))
    fi
done
if [[ $(grep -c . <<<"$LAST_OUT") -ne 8 ]]; then
    echo "FAIL expected exactly 8 hits, got:"
    echo "$LAST_OUT" | sed 's/^/    /'
    failures=$((failures + 1))
fi

# Removed and context lines never fail, and near misses are not leaks.
cat >"$work/clean.diff" <<EOF
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3,2 +3,6 @@
-let path = "${home_linux}x";
-let addr = "$lan_192";
+let version = "1${lan_10}";
+let public = "$below_cgnat and 100.128.1.1 and $above_private and 8.8.8.8";
+let placeholder = "/home/<name>/ and /Users/\$USER/ and <tailnet>.ts.net";
+let loopback = "127.0.0.1";
EOF
check "removed lines, near misses and placeholders pass" 0 "$work/clean.diff"

# A home directory that ends the path: at the end of a line, before a quote, a space or a colon.
cat >"$work/bare.diff" <<EOF
diff --git a/docs/env.md b/docs/env.md
--- a/docs/env.md
+++ b/docs/env.md
@@ -1,0 +1,5 @@
+HOME=$bare_linux
+"cwd": "$bare_mac", 'x'
+cd $bare_linux && ls
+PATH=$bare_mac:/usr/bin
+a placeholder /home/user and /Users/runner is fine
EOF
check "a bare home directory fails" 1 "$work/bare.diff"
for want in \
    "docs/env.md:1: home path: $bare_linux" \
    "docs/env.md:2: home path: $bare_mac" \
    "docs/env.md:3: home path: $bare_linux" \
    "docs/env.md:4: home path: $bare_mac"; do
    if ! grep -qxF "$want" <<<"$LAST_OUT"; then
        echo "FAIL missing hit: $want"
        failures=$((failures + 1))
    fi
done
if [[ $(grep -c . <<<"$LAST_OUT") -ne 4 ]]; then
    echo "FAIL expected exactly 4 bare hits, got:"
    echo "$LAST_OUT" | sed 's/^/    /'
    failures=$((failures + 1))
fi

# A snapshot is scanned like any file: only upstream's named authors pass in one.
cat >"$work/snap.diff" <<EOF
diff --git a/zellij-server/src/unit/snapshots/x.snap b/zellij-server/src/unit/snapshots/x.snap
--- a/zellij-server/src/unit/snapshots/x.snap
+++ b/zellij-server/src/unit/snapshots/x.snap
@@ -1,0 +1,2 @@
+cwd ${home_linux}src
+cwd ${upstream_author}src
EOF
check "a snapshot with a home path fails" 1 "$work/snap.diff"
if [[ $LAST_OUT != "zellij-server/src/unit/snapshots/x.snap:1: home path: $home_linux" ]]; then
    echo "FAIL snapshot: expected only the first line, got:"
    echo "$LAST_OUT" | sed 's/^/    /'
    failures=$((failures + 1))
fi

# The allowlist lets a known fixture through, and only that one.
cat >"$work/allow.txt" <<EOF
^src/tests/fixtures/
:/(home|Users)/zed-q/\$
EOF
cat >"$work/allowed.diff" <<EOF
diff --git a/src/tests/fixtures/recording b/src/tests/fixtures/recording
new file mode 100644
--- /dev/null
+++ b/src/tests/fixtures/recording
@@ -0,0 +1 @@
+prompt at $lan_192
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1,0 +2,2 @@
+see ${home_linux}notes
+and $cgnat
EOF
LEAK_SCAN_ALLOWLIST="$work/allow.txt" check "the allowlist passes fixtures and named paths" 1 \
    "$work/allowed.diff"
if [[ $LAST_OUT != "README.md:3: Tailscale CGNAT address: $cgnat" ]]; then
    echo "FAIL allowlist: expected only the CGNAT hit, got:"
    echo "$LAST_OUT" | sed 's/^/    /'
    failures=$((failures + 1))
fi

# A commit range through git, in a scratch repository.
repo="$work/repo"
git -c core.hooksPath=/dev/null init -q --template= "$repo"
# No hooks: a developer's global commit hooks must not judge the crafted leaks.
repo_commit() {
    git -C "$repo" -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@example.invalid \
        commit -q "$@"
}
repo_commit -q --allow-empty -m base
echo "upstream had ${home_linux}x" >"$repo/old.txt"
git -C "$repo" add old.txt
repo_commit -q -m upstream
base=$(git -C "$repo" rev-parse HEAD)
echo "clean addition" >>"$repo/old.txt"
repo_commit -q -am clean
clean=$(git -C "$repo" rev-parse HEAD)
echo "leak at $lan_10" >"$repo/new.txt"
git -C "$repo" add new.txt
repo_commit -q -m leak
leak=$(git -C "$repo" rev-parse HEAD)
range_check() {
    local name=$1 want=$2 status=0
    shift 2
    (cd "$repo" && "$scan" "$@" >/dev/null 2>&1) || status=$?
    if [[ $status -ne $want ]]; then
        echo "FAIL $name: exit $status, want $want"
        failures=$((failures + 1))
    else
        echo "ok   $name"
    fi
}
range_check "a range adding only clean lines passes over old leaks" 0 "$base" "$clean"
range_check "a range adding a leak fails" 1 "$clean" "$leak"
range_check "an unknown commit is a usage error" 2 0000000000000000000000000000000000000000 "$leak"

# A commit message is scanned like an added line.
repo_commit -q --allow-empty -m "clean subject" -m "built in $bare_linux"
told=$(git -C "$repo" rev-parse HEAD)
range_check "a leak in a commit message fails" 1 "$leak" "$told"
range_check "a range before that message passes" 0 "$base" "$clean"

# git failing mid-scan fails the scan; it never reads as nothing added.
echo "an unreadable blob" >"$repo/broken.txt"
git -C "$repo" add broken.txt
repo_commit -q -m broken
broken=$(git -C "$repo" rev-parse HEAD)
blob=$(git -C "$repo" rev-parse HEAD:broken.txt)
[[ ${#blob} -eq 40 ]] || { echo "FAIL no blob id"; exit 1; }
loose="${repo:?}/.git/objects/${blob:0:2}/${blob:2}"
[[ -f $loose ]] || { echo "FAIL no loose object at $loose"; exit 1; }
rm -f -- "$loose"
range_check "a diff git cannot read is an error, not a pass" 2 "$told" "$broken"

if [[ $failures -ne 0 ]]; then
    echo "$failures failure(s)"
    exit 1
fi
echo "all leak-scan tests passed"
