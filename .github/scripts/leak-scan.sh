#!/usr/bin/env bash
# Fails when a change ADDS a local-context leak: a home directory path, a private (RFC1918) or
# Tailscale CGNAT IPv4 address (the 100.64/10 block), or a `*.ts.net` name.
#
# Only added lines are scanned, so content already in the tree (upstream's own fixtures) never
# fails a change that does not touch it.
#
# Usage:
#   leak-scan.sh <base> <head>     scan what <head> adds over <base> (two commits)
#   leak-scan.sh --diff <file>     scan a unified diff ('-' reads stdin)
#
# The allowlist (default: leak-scan-allowlist.txt beside this script, override with
# LEAK_SCAN_ALLOWLIST) holds one Perl regex per line, matched against `<path>:<match>`. A hit that
# matches any of them is let through. Blank lines and lines starting with '#' are ignored.
#
# Exit status: 0 clean, 1 a leak was found, 2 usage or git error.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
allowlist="${LEAK_SCAN_ALLOWLIST:-$here/leak-scan-allowlist.txt}"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2
    exit 2
}

if [[ $# -eq 2 && $1 == --diff ]]; then
    if [[ $2 == - ]]; then
        diff_input=/dev/stdin
    else
        diff_input=$2
    fi
    exec 3<"$diff_input"
elif [[ $# -eq 2 ]]; then
    for rev in "$1" "$2"; do
        if ! git rev-parse --quiet --verify "$rev^{commit}" >/dev/null; then
            echo "leak-scan: not a commit here: $rev (is the checkout deep enough?)" >&2
            exit 2
        fi
    done
    exec 3< <(git diff --no-color --no-ext-diff -U0 --no-renames "$1" "$2" --)
else
    usage
fi

ALLOWLIST="$allowlist" perl -e '
    use strict;
    use warnings;

    my @allow;
    if (open(my $fh, "<", $ENV{ALLOWLIST})) {
        while (my $line = <$fh>) {
            chomp $line;
            next if $line =~ /^\s*(#|$)/;
            push @allow, qr/$line/;
        }
    }

    my $octet = qr/(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)/;
    my @rules = (
        ["home path", qr{/home/[A-Za-z_][A-Za-z0-9_.-]*/|/Users/[A-Za-z0-9_][A-Za-z0-9_.-]*/}],
        ["private IPv4 address", qr{(?<![\d.])(?:10\.$octet|172\.(?:1[6-9]|2\d|3[01])|192\.168)\.$octet\.$octet(?![\d.])}],
        ["Tailscale CGNAT address", qr{(?<![\d.])100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.$octet\.$octet(?![\d.])}],
        ["tailnet name", qr{(?<![A-Za-z0-9.-])[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.ts\.net\b}],
    );

    # A file header is "--- a/..." then "+++ b/...". Outside a hunk only; inside one, a line
    # that starts with "+++" is an added line whose text starts with "++".
    my ($path, $line_no, $hits, $in_hunk, $prev) = ("", 0, 0, 0, "");
    while (my $line = <STDIN>) {
        my $was = $prev;
        $prev = $line;
        if ($line =~ /^diff /) {
            $in_hunk = 0;
            next;
        }
        if (!$in_hunk && $was =~ /^--- / && $line =~ m{^\+\+\+ (?:b/)?(.*?)\s*$}) {
            $path = $1;
            next;
        }
        if ($line =~ /^@@ -\S+ \+(\d+)/) {
            $line_no = $1;
            $in_hunk = 1;
            next;
        }
        next unless $in_hunk;
        if ($line =~ /^\+/) {
            my $text = substr($line, 1);
            chomp $text;
            # A cheap test first: most lines hold no fixed part of any rule.
            if ($text =~ m{/home/|/Users/|\.ts\.net|(?:10|172|192|100)\.\d}) {
            for my $rule (@rules) {
                my ($kind, $re) = @$rule;
                while ($text =~ /($re)/g) {
                    my $match = $1;
                    my $key = "$path:$match";
                    next if grep { $key =~ $_ } @allow;
                    print "$path:$line_no: $kind: $match\n";
                    $hits++;
                }
            }
            }
            $line_no++;
        }
    }
    if ($hits) {
        print STDERR "leak-scan: $hits added line match(es) look like local context. Replace "
            . "each with a placeholder, or add a regex to the allowlist if it is a known "
            . "upstream fixture.\n";
        exit 1;
    }
    exit 0;
' <&3
